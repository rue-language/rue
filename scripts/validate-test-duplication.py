#!/usr/bin/env python3
"""Fail when one test is scheduled more than once in a CI run (ADR-0069 §2).

Every other CI gate in this repository compares *target lists*.
`//:cli-shard-coverage-validation` compares BUCK's shard targets with the
matrix; `scripts/validate-ci-gate.py` compares the workflow's jobs with the
platform responsibility matrix; `//test_tiers.bxl:validate` compares tier
labels with the test graph. None of them compares *test contents*, so a target
that is a strict superset of another is invisible to all of them:
`//crates/rue-compiler:scaling-matrix-test` re-ran 813 of `rue-compiler-test`'s
tests for weeks to gain three, in the same lane, and every gate stayed green
(RUE-1262). This gate closes that class rather than that instance.

The invariant, from ADR-0069 §2: **no unit of work executes more than once per
platform per run without a declared reason.** Cross-platform repetition is
governed by the same ledger. The broad compiler suite is now linux-premerge
only; native compiler host assertions are a separate graph-owned target.

How it works:

1. **Lane membership comes from the live graph, never from YAML.** The premerge
   lane is `attrfilter(labels, 'rue_test_tier_premerge', ...)` minus the
   `rue_cli_shard`, `rue_ci_dedicated_lane`, and `rue_ci_clippy_lane` sets —
   the derivation `docs/notes/rue-1250-premerge-critical-path.md` measured and
   `test.sh` executes. The corpus and gated-lane inventories come from
   `scripts/affected-targets`, which is the same list CI's determinator already
   consults, so this gate adds no new hand-maintained row to ADR-0069's ledger.
2. **Identities come from `--list` on the test binaries.** Cheap and exact: it
   is how the superset above was found. Rust unit targets, the `sh_test`
   wrappers that select rows out of one (the scaling matrix), and the
   `cached_corpus_suite` harnesses all speak the libtest `--list` protocol, and
   each is listed with the exact args and env its own Buck target carries.
3. **Allowances are declared here with a reason.** Absence never implies
   permission, and an allowance that stops matching anything is an error, so
   the ledger cannot rot into a list of things that used to be true.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BUCK = ROOT / "buck2"
AFFECTED_TARGETS = ROOT / "scripts" / "affected-targets"

TIER_PREMERGE = "rue_test_tier_premerge"
CLI_SHARD_LABEL = "rue_cli_shard"
DEDICATED_LANE_LABEL = "rue_ci_dedicated_lane"
CLIPPY_LANE_LABEL = "rue_ci_clippy_lane"

PREMERGE_QUERY = (
    "attrfilter(labels, '" + TIER_PREMERGE + "', set(//... toolchains//...))"
)

# Which platform executes which lane. `linux-premerge` and `platform-corpus`
# are the two halves of the linux-x64 test load: the premerge tier minus the
# corpora and clippy gates that own their own job, and those owned lanes. The
# rest are gated selection proxies named by RUE-1130.
LANE_PLATFORMS = {
    "linux-premerge": "linux-x64",
    "platform-corpus": "linux-x64",
    "clippy": "linux-x64",
    "release": "linux-x64",
    "native-linux-arm64": "linux-arm64",
    "native-macos-arm64": "macos-arm64",
}

# Gated lanes that schedule no Buck test target of their own. Their
# `lane_targets` entries are *selection proxies* — the library and binary a
# script consumes — not tests, so listing them would invent duplicates that no
# runner executes. Keeping them named here rather than filtered by rule kind is
# deliberate: `valgrind` names `//:cli-tests`, which IS a test target, and it
# runs generated programs under memcheck rather than that corpus.
SELECTION_PROXY_LANES = {
    "valgrind",
    "asan",
    "compiler-reproducibility",
    "rue-program-digests",
}

# The native lanes run three CLI selections as workflow steps rather than as
# Buck test targets. Everything but their arguments is derived from the
# alias's own graph node; `scripts/validate-ci-gate.py` imports these so the
# workflow and this gate cannot disagree about what runs.
NATIVE_CLI_ALIAS = "//crates/rue-cli-tests:cli"
AGGREGATE_ABI_DIFFERENTIAL = "cli.differential_opt::aggregate_abi_across_opt_levels"
NATIVE_CLI_INVOCATIONS = (
    ("abi", "--skip", AGGREGATE_ABI_DIFFERENTIAL),
    ("cli.linker",),
    ("cli.fs_file_io",),
)


@dataclass(frozen=True)
class Allowance:
    """A duplication this repository has decided to keep, and why.

    An entry declares one of exactly two shapes, and covers only that shape:

    * `per-target` — each named target repeats *by itself* across `platforms`.
      One entry can carry a roster of targets that repeat for the same written
      reason, because each of them is an independent one-target fact.
    * `between-targets` — precisely the named targets overlap on `platforms`.
      The match is on the exact set.

    Neither shape ever matches by subset. An earlier revision allowed a
    duplicate set whose targets were a subset of an entry's, which quietly
    absorbed two real defects: an overlap *between* two members of a
    `per-target` roster (the RUE-1262 shape, reproduced inside an allowance),
    and — because CLI shards attribute to the corpus they slice — an overlap
    between two shards, which reduces to the single owner `//:cli-tests` and
    slid under the two-target `release-smoke` entry. Both now fail.
    """

    targets: tuple[str, ...]
    platforms: tuple[str, ...]
    reason: str
    kind: str = "between-targets"
    def covers(self, duplicate: "DuplicateSet") -> bool:
        if duplicate.platforms != self.platforms:
            return False
        if self.kind == "per-target":
            return len(duplicate.targets) == 1 and duplicate.targets[0] in self.targets
        return set(duplicate.targets) == set(self.targets)

    def unmatched(self, covered: list["DuplicateSet"]) -> list[str]:
        """What this entry still claims that nothing supports.

        A `between-targets` entry is one indivisible claim about one set, so it
        is stale as a whole. A roster is N independent claims: seven of eight
        targets could stop running while the entry stayed "matched" by the
        eighth, and seven written reasons would keep vouching for work nobody
        does, so it goes stale per target.
        """
        if self.kind != "per-target":
            return [] if covered else [", ".join(self.targets)]
        alive = {duplicate.targets[0] for duplicate in covered}
        return [target for target in self.targets if target not in alive]


ALLOWANCES = (
    Allowance(
        kind="per-target",
        targets=(
            "//crates/rue-allocator:rue-allocator-test",
            "//crates/rue-codegen:rue-codegen-test",
            "//crates/rue-linker:rue-linker-test",
            "//crates/rue-runtime-abi:rue-runtime-abi-test",
            "//crates/rue-runtime:rue-runtime-test",
            "//crates/rue-runtime:runtime-archives-test",
            "//crates/rue-target:rue-target-test",
            "//fixtures/rue-program:hello-runs-test",
            "//crates/rue-compiler:rue-compiler-platform-native-test",
        ),
        platforms=("linux-arm64", "linux-x64", "macos-arm64"),
        reason=(
            "The platform responsibility matrix in docs/process/ci.md requires "
            "exactly this: the native lanes prove the host ABI, object/linker "
            "path, runtime archive, syscalls, and platform behaviour that "
            "cross-compilation cannot, and a pass on x86-64 Linux is not "
            "evidence for AArch64 or Mach-O. The repetition is the coverage. "
            "The focused compiler host-conditional target is included because "
            "its ignored platform_native_ rows intentionally execute on Linux "
            "premerge and both native hosts; the broad compiler test selection "
            "is Linux-only. "
            "Each target here repeats on its own; an overlap BETWEEN any two of "
            "them is a different fact and is not covered by this entry."
        ),
    ),
    Allowance(
        targets=("//:cli-tests", "//:release-smoke"),
        platforms=("linux-x64",),
        reason=(
            "The CLI shards run these cases under //platforms:debug and the "
            "release job runs //:release-smoke under //platforms:release. The "
            "release-configured execution is intentionally retained because it "
            "proves the release optimization configuration; //:release-smoke "
            "is marked rue_ci_dedicated_lane so linux-premerge no longer runs a "
            "third debug execution. The exact overlap remains declared because "
            "debug and release configurations are deliberately distinct."
        ),
    ),
    Allowance(
        targets=("//:cli-tests", "//crates/rue-cli-tests:cli"),
        platforms=("linux-arm64", "linux-x64", "macos-arm64"),
        reason=(
            "The native lanes run `scripts/rue cli abi` with one exact skip, "
            "plus `cli.linker` and `cli.fs_file_io` — the developer entry point, "
            "with neither "
            "RUE_CLI_CASE_TIER nor RUE_PLATFORM_CASE_SELECTION set, so every "
            "matching case of every tier runs. Those sections contain "
            "native-execution cases with empty `only_on` lists, which is "
            "exactly why docs/process/ci.md keeps the explicit filters beside "
            "the self-enrolling `only_on` selection: a real ABI, linker, or "
            "filesystem program proves the host syscall and object path, and "
            "the x86-64 Linux pass in the CLI shards is not evidence for it. "
            "The repetition is the coverage the responsibility matrix asks for."
        ),
    ),
)


# ADR-0069 §2 / RUE-1265. Targets whose harness is a Rust binary — so the gate
# expects it to answer `--list` — and which do not. An undeclared one is an
# error, because the alternative is the failure this gate cannot absorb: a
# binary that stops listing collapses hundreds of tests into one opaque unit
# and CI prints a clean pass. Each entry states what its opacity hides.
#
# Harnesses that are not Rust binaries are not in this ledger and are never
# probed: a `sh_binary` cannot be libtest, and asking one to `--list` runs its
# whole suite (see `Listing.listable`).
NOT_LISTABLE = {
    "//crates/rue-oracle-diff:oracle-diff-test": (
        "distinct-assertion opaque target: rue-oracle-diff owns its own argument "
        "grammar and exits 1 on --list. It drives every runnable CLI case through "
        "the reference interpreter and compares that result with the compiler. "
        "The interpreter differential is a distinct assertion, not repeated "
        "coverage, so this target is intentionally excluded from ordinary "
        "duplicate accounting. Its runtime eligibility and corpus selection are "
        "harness-owned, making a --list inventory non-authoritative; keeping it "
        "opaque avoids running the corpus merely to classify it."
    ),
    "//crates/rue-oracle-diff:oracle-diff-test-o2": (
        "distinct-assertion opaque target: the O2 shard runs the complete modeled CLI "
        "corpus through the reference interpreter and optimized native compiler. Its "
        "case inventory is harness-owned, and the separate optimization result is a "
        "distinct assertion rather than duplicate execution."
    ),
    "//crates/rue-oracle-diff:oracle-diff-test-o3": (
        "distinct-assertion opaque target: the O3 shard runs the complete modeled CLI "
        "corpus through the reference interpreter and optimized native compiler. Its "
        "case inventory is harness-owned, and the separate optimization result is a "
        "distinct assertion rather than duplicate execution."
    ),
    "//crates/rue-oracle-diff:oracle-diff-spec-test": (
        "distinct-assertion opaque target: the harness drives the specification "
        "corpus through the reference interpreter and compares compiler behavior. "
        "That interpreter differential is a distinct assertion, not repeated "
        "coverage, and therefore is not ordinary duplicate accounting. The "
        "harness owns runtime eligibility and argument grammar, so --list would "
        "not provide an authoritative inventory; it remains intentionally opaque."
    ),
    "//crates/rue-oracle-diff:oracle-diff-spec-test-o2": (
        "distinct-assertion opaque target: the O2 shard runs the complete modeled spec "
        "corpus through the reference interpreter and optimized native compiler. Its "
        "case inventory is harness-owned, and the separate optimization result is a "
        "distinct assertion rather than duplicate execution."
    ),
    "//crates/rue-oracle-diff:oracle-diff-spec-test-o3": (
        "distinct-assertion opaque target: the O3 shard runs the complete modeled spec "
        "corpus through the reference interpreter and optimized native compiler. Its "
        "case inventory is harness-owned, and the separate optimization result is a "
        "distinct assertion rather than duplicate execution."
    ),
    "//:spec-traceability": (
        "rue-spec's `--traceability` is a reporting mode, not a filter: handed "
        "`--list` as well it prints the traceability report and exits 0. It "
        "owns no cases of its own — it reads the same corpus //:spec-tests "
        "runs and checks paragraph coverage — so the opacity hides no second "
        "execution of anything. It is skipped rather than probed because "
        "probing it IS a second execution of a premerge suite."
    ),
    "//:compiler-spec-machine-index": (
        "rue-spec's `--check-machine-index` is a hermetic metadata drift check, "
        "not a test filter: it generates the compiler/specification index twice "
        "and compares the deterministic bytes, then exits before constructing "
        "the libtest harness. Its opacity hides no repeated test inventory and "
        "executes no spec cases. Passing `--list` cannot enumerate anything and "
        "would instead run the complete drift assertion, so the target remains "
        "an explicit opaque unit."
    ),
    "//:frontend-diff-test": (
        "rue-frontend-diff takes only --refresh-manifest and drives a corpus "
        "recorded in crates/rue-frontend-diff/src/corpus_manifest.rs. Its "
        "corpus is that manifest rather than a case inventory shared with "
        "another target, so the opacity hides no known overlap."
    ),
}


@dataclass(frozen=True)
class Scheduled:
    """One target's test identities, as one lane on one platform runs them."""

    platform: str
    lane: str
    target: str
    # The target a duplicate set is attributed to. CLI shards fold onto the
    # canonical corpus they slice, so the ledger does not churn every time the
    # measured weights move a case from one shard to another.
    owner: str
    identities: frozenset[str]


@dataclass(frozen=True)
class DuplicateSet:
    targets: tuple[str, ...]
    platforms: tuple[str, ...]
    identities: tuple[str, ...]
    schedulings: tuple[str, ...]
    # How many executions each platform pays. `linux-x64 ×2` is the strict
    # "twice per platform per run" violation; a run of ×1 across three
    # platforms is cross-platform repetition. Both are in the ledger, and a
    # reader should not have to count the `scheduled by` line to tell them
    # apart.
    per_platform: tuple[tuple[str, int], ...] = ()

    def describe(self, examples: int = 3) -> str:
        # An identity is `<namespace>\t<test name>`; the namespace exists so two
        # crates can both own `tests::empty_program`, and a reader does not need
        # to see it.
        names = [identity.split("\t", 1)[-1] for identity in self.identities]
        shown = ", ".join(names[:examples])
        if len(names) > examples:
            shown += f", … ({len(names) - examples} more)"
        spread = ", ".join(f"{platform} ×{count}" for platform, count in self.per_platform)
        return (
            f"{len(self.identities)} test(s) scheduled more than once ({spread})"
            f"\n    owning targets: {', '.join(self.targets)}"
            f"\n    scheduled by:   {'; '.join(self.schedulings)}"
            f"\n    tests:          {shown}"
        )


def duplicate_sets(scheduled: list[Scheduled]) -> list[DuplicateSet]:
    """Group every identity scheduled more than once by who owns it.

    The grouping key is (owning targets, platforms) rather than the individual
    test, because that is the unit a reader can act on: "these two targets
    overlap on this platform" is a fact about the graph, while "this test runs
    twice" is 813 facts about one.

    Attribution folds a CLI shard onto the corpus it slices so a written
    allowance does not expire when measured weights move a case between shards.
    That fold must not erase the very thing it is folding, though: when several
    distinct targets collapse to one owner, the duplication is *inside* the
    fold group — two shards running the same case — and the key reverts to the
    real targets. Otherwise a genuine cross-shard overlap would present as the
    single owner `//:cli-tests` and no report could name what caused it.
    """
    where: dict[str, list[Scheduled]] = defaultdict(list)
    for entry in scheduled:
        for identity in entry.identities:
            where[identity].append(entry)

    grouped: dict[tuple, dict] = defaultdict(
        lambda: {"identities": set(), "schedulings": set(), "spread": {}}
    )
    for identity, entries in where.items():
        if len(entries) < 2:
            continue
        owners = {entry.owner for entry in entries}
        targets = {entry.target for entry in entries}
        attributed = targets if len(owners) == 1 and len(targets) > 1 else owners
        key = (
            tuple(sorted(attributed)),
            tuple(sorted({entry.platform for entry in entries})),
        )
        body = grouped[key]
        body["identities"].add(identity)
        per_identity: dict[str, int] = defaultdict(int)
        for entry in entries:
            body["schedulings"].add(f"{entry.target} in {entry.lane} ({entry.platform})")
            per_identity[entry.platform] += 1
        # The worst single test, not the group's total: three sibling filters
        # each owning disjoint cases is `×1` on each platform, and reporting it
        # as `×3` would claim a same-platform violation that does not exist.
        for platform, count in per_identity.items():
            body["spread"][platform] = max(body["spread"].get(platform, 0), count)

    return sorted(
        (
            DuplicateSet(
                targets=targets,
                platforms=platforms,
                identities=tuple(sorted(body["identities"])),
                schedulings=tuple(sorted(body["schedulings"])),
                per_platform=tuple(
                    (platform, body["spread"][platform]) for platform in platforms
                ),
            )
            for (targets, platforms), body in grouped.items()
        ),
        key=lambda duplicate: (duplicate.targets, duplicate.platforms),
    )


def review(
    duplicates: list[DuplicateSet], allowances: tuple[Allowance, ...] = ALLOWANCES
) -> list[str]:
    """Errors for undeclared duplications, and for allowances that rotted.

    Matching is exact in both directions — see `Allowance` for the two shapes
    and for the two defects that subset matching hid.
    """
    errors: list[str] = []
    covered_by: dict[int, list[DuplicateSet]] = {
        index: [] for index in range(len(allowances))
    }

    for duplicate in duplicates:
        match = next(
            (
                index
                for index, allowance in enumerate(allowances)
                if allowance.covers(duplicate)
            ),
            None,
        )
        if match is not None:
            covered_by[match].append(duplicate)
            continue
        errors.append(
            "undeclared duplication: "
            + duplicate.describe()
            + "\n    Either stop scheduling one of them, or add an Allowance to "
            "scripts/validate-test-duplication.py naming every target involved "
            "and the reason the second execution earns its cost."
        )

    for index, allowance in enumerate(allowances):
        for subject in allowance.unmatched(covered_by[index]):
            errors.append(
                f"stale allowance: {subject} on "
                f"{', '.join(allowance.platforms)} no longer duplicates "
                "anything. Remove it from the entry; an allowance that matches "
                "nothing is a claim nobody is checking."
            )

    return errors


# --------------------------------------------------------------------------
# Buck graph interrogation
# --------------------------------------------------------------------------

MACRO_RE = re.compile(r"\$\((location|exe|exe_target)\s+([^)]+)\)")


def normalize(label: str) -> str:
    """`root//pkg:name` and `//pkg:name` are the same target."""
    label = label.strip()
    if label.startswith("root//"):
        return label[len("root") :]
    return label


class Buck:
    def __init__(self, buck: Path, root: Path) -> None:
        self.buck = buck
        self.root = root

    def _run(self, args: list[str]) -> str:
        result = subprocess.run(
            [str(self.buck), *args],
            cwd=str(self.root),
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"buck2 {' '.join(args)} failed ({result.returncode}):\n{result.stderr}"
            )
        return result.stdout

    def uquery(self, expression: str, attributes: str) -> dict[str, dict]:
        raw = self._run(
            ["uquery", expression, f"--output-attribute={attributes}", "--json"]
        )
        return {normalize(key): value for key, value in json.loads(raw).items()}

    def outputs(self, targets: list[str]) -> dict[str, str]:
        """Absolute default-output path per target, building what is missing."""
        if not targets:
            return {}
        raw = self._run(["build", "--show-full-json-output", *targets])
        # `buck2 build` prints progress on stderr and the JSON map on stdout,
        # but a warning can still precede it; take the JSON object only.
        start = raw.find("{")
        parsed = json.loads(raw[start:]) if start >= 0 else {}
        return {normalize(key): value for key, value in parsed.items()}


ATTRIBUTES = "^(buck.type|labels|test|args|env|tests|harness|harness_args|corpus_env|srcs|crate_root)$"


@dataclass
class Listing:
    """How to enumerate one target's tests, once its inputs are materialized."""

    target: str
    binary: str  # the Buck target providing the executable
    args: list[str]
    env: dict[str, str]
    namespace_key: str  # targets sharing this share a test-name namespace
    # Only `rust_test` binaries carry rustc's libtest, which is the only
    # harness here with an `#[ignore]` concept. The corpus harnesses are
    # libtest2-mimic `rust_binary`s, so asking them for the ignored set costs a
    # second full discovery and can only return what the first listing already
    # returned.
    has_ignored: bool = False


@dataclass
class Inventory:
    listings: dict[str, Listing] = field(default_factory=dict)
    # Targets whose contents cannot be enumerated (a shell gate, a rue_program
    # scenario). They are still units of work, so each contributes exactly one
    # opaque identity and duplicates across lanes are still caught.
    opaque: set[str] = field(default_factory=set)
    # What each scheduled target expands to. A `test_suite` is not a unit of
    # work — buck2 runs its members, once each, no matter how many suites reach
    # them — so the schedule is built from these leaves rather than from the
    # names the lane happens to mention.
    units: dict[str, list[str]] = field(default_factory=dict)
    # Targets that name a Buck target as their harness and whose attributes
    # could not be read. Falling back to "opaque" here would be the one failure
    # mode this gate cannot tolerate: a wrapper that lists 902 tests would
    # silently count as one unit and its duplication would disappear. Loud
    # instead.
    unresolved: dict[str, str] = field(default_factory=dict)
    # Targets the graph says cannot be libtest, with the rule kind that settles
    # it. Reported, never probed — see the comment at the assignment site.
    not_libtest: dict[str, str] = field(default_factory=dict)
    # Targets whose listing was well formed and enumerated nothing.
    empty: set[str] = field(default_factory=set)


def is_target_label(value: object) -> bool:
    return isinstance(value, str) and value.startswith("root//") and ":" in value


def namespace_key(binary: str, node: dict) -> str:
    """Targets compiling the same sources share a test-name namespace.

    Two crates can both own `tests::empty_program` without that being a
    duplication, so identities are namespaced. Keying the namespace on the
    sources rather than on the label is what catches the RUE-1262 shape: a
    second `rust_test` over the same `srcs` is a different target compiling the
    same tests, and it must collide. It is also what makes the CLI shards, the
    release smoke, and the native lanes' `scripts/rue cli` steps comparable —
    all of them run one binary.
    """
    srcs = node.get("srcs")
    if srcs:
        fingerprint = json.dumps(
            {
                "srcs": sorted(normalize(str(src)) for src in srcs),
                "crate_root": node.get("crate_root"),
            },
            sort_keys=True,
        )
        digest = hashlib.sha256(fingerprint.encode()).hexdigest()[:12]
        return f"srcs:{digest}"
    return f"target:{binary}"


def resolve_inventory(buck: Buck, targets: set[str]) -> Inventory:
    """Classify each scheduled target into a listable harness or an opaque unit."""
    inventory = Inventory()
    pending = set(targets)
    seen: set[str] = set()
    attributes: dict[str, dict] = {}

    # test_suite membership is transitive, so resolve in rounds rather than
    # assuming one level.
    while pending:
        batch = sorted(pending - seen)
        seen |= set(batch)
        pending = set()
        # A sub-target (`//crates/rue-rir:rue-rir[doc]`, the compile-fail doc
        # suite) is not queryable and owns no listable inventory; it is one
        # opaque unit of work, which is all this gate needs of it.
        inventory.opaque |= {target for target in batch if "[" in target}
        batch = [target for target in batch if "[" not in target]
        if not batch:
            break
        attributes.update(buck.uquery(f"set({' '.join(batch)})", ATTRIBUTES))
        for target in batch:
            node = attributes.get(target)
            if node is None:
                continue
            if node.get("buck.type") == "test_suite":
                members = {normalize(item) for item in node.get("tests") or []}
                pending |= members
                attributes.setdefault(target, node)["_members"] = sorted(members)

    # A second query for the two things a listable target points at: the binary
    # a wrapper runs, and the corpus action behind a stamp check. Everything
    # else a gate names is an input, not a harness.
    requested: set[str] = set()

    def reference_round() -> bool:
        referenced: set[str] = set()
        for _, node in attributes.items():
            test = node.get("test")
            if is_target_label(test):
                referenced.add(normalize(test))
            harness = node.get("harness")
            if is_target_label(harness):
                referenced.add(normalize(harness))
            for arg in node.get("args") or []:
                for _, payload in MACRO_RE.findall(arg):
                    candidate = normalize(payload)
                    if candidate.startswith("//") and candidate.endswith("-action"):
                        referenced.add(candidate)
        referenced = {t for t in referenced if "[" not in t} - set(attributes) - requested
        if not referenced:
            return False
        # Record the request, not the answer: a target the query cannot return
        # never enters `attributes`, and asking again forever is not a better
        # outcome than classifying it as unresolved.
        requested.update(referenced)
        attributes.update(buck.uquery(f"set({' '.join(sorted(referenced))})", ATTRIBUTES))
        return True

    # Two rounds: a stamp-check `sh_test` names its `-action`, and only that
    # action names the harness. Stopping after one round left every corpus
    # harness unqueried, which reads as "no rule kind" — indistinguishable from
    # "not a Rust binary" — and namespaced its cases by label instead of by
    # sources, so the CLI shards and //:release-smoke could not collide.
    while reference_round():
        pass

    def namespace_of(binary: str) -> str:
        return namespace_key(binary, attributes.get(binary, {}))

    def classify(target: str) -> list[str]:
        """Returns the leaf units `target` schedules, registering each one."""
        node = attributes.get(target)
        if node is None:
            inventory.opaque.add(target)
            return [target]
        kind = node.get("buck.type")

        if kind == "test_suite":
            leaves: list[str] = []
            for member in node.get("_members", []):
                leaves.extend(classify(member))
            return leaves

        if kind == "rust_test":
            inventory.listings[target] = Listing(
                target=target,
                binary=target,
                args=[],
                env=dict(node.get("env") or {}),
                namespace_key=namespace_of(target),
                has_ignored=True,
            )
            return [target]

        if kind == "sh_test":
            test = node.get("test")
            args = list(node.get("args") or [])
            if is_target_label(test):
                wrapped = normalize(test)
                wrapped_kind = attributes.get(wrapped, {}).get("buck.type")
                if wrapped_kind in ("rust_test", "rust_binary"):
                    # The scaling matrix: one binary, selected rows.
                    inventory.listings[target] = Listing(
                        target=target,
                        binary=wrapped,
                        args=args,
                        env=dict(node.get("env") or {}),
                        namespace_key=namespace_of(wrapped),
                        has_ignored=wrapped_kind == "rust_test",
                    )
                    return [target]
                action = corpus_action(args)
                if action and attributes.get(action, {}).get("buck.type") == "_corpus_action":
                    node_action = attributes[action]
                    harness = normalize(node_action["harness"])
                    harness_kind = attributes.get(harness, {}).get("buck.type")
                    # THE `--list` PROTOCOL IS NOT UNIVERSAL, AND PROBING FOR IT
                    # IS NOT FREE. `//:reproducible-programs` and
                    # `//:oracle-diff-generated-smoke` are `sh_binary` harnesses
                    # over shell scripts that do no argument handling at all:
                    # handed `--list` they ignore it, run their entire suite to
                    # completion, exit 0, and print nothing this parser matches.
                    # The first revision of this gate probed them anyway. It
                    # bought zero identities for 29s of the 30.6s it spent, and
                    # — because both are `rue_test_tier_premerge` and the probe
                    # bypasses `cached_corpus_suite` — it ran two premerge
                    # suites a second time, uncached, in the step before
                    # `test.sh` ran them again. A duplication gate causing
                    # duplication. Only a Rust binary can be libtest, so that is
                    # the question the graph is asked, before anything executes.
                    if harness_kind not in ("rust_test", "rust_binary"):
                        inventory.not_libtest[target] = (
                            f"harness {harness} is a {harness_kind}; only a Rust "
                            "binary can carry libtest"
                        )
                        inventory.opaque.add(target)
                        return [target]
                    inventory.listings[target] = Listing(
                        target=target,
                        binary=harness,
                        args=list(node_action.get("harness_args") or []),
                        env=dict(node_action.get("corpus_env") or {}),
                        namespace_key=namespace_of(harness),
                    )
                    return [target]
                if wrapped not in attributes:
                    inventory.unresolved[target] = (
                        f"runs {wrapped}, whose attributes the graph query did "
                        "not return"
                    )
        inventory.opaque.add(target)
        return [target]

    def corpus_action(args: list[str]) -> str | None:
        for arg in args:
            for _, payload in MACRO_RE.findall(arg):
                candidate = normalize(payload)
                if candidate.endswith("-action"):
                    return candidate
        return None

    for target in sorted(targets):
        inventory.units[target] = classify(target)
    return inventory


# Env keys a harness reads only while *running* a case, never while
# discovering one. Materializing them would make the gate build artifacts no
# listing consults.
#
# `RUE_CLI_STAGED_PROGRAMS` is the whole reason this exists.
# `//:cli-staged-programs` stages ten `rue_program` executables, Meridian's
# among them at 80.7s in ADR-0070's own table, and the premerge lane never
# builds it: that job builds `//crates/...`, and root corpus targets are
# deliberately excluded (`ci.yml`, "Build all targets"). The CLI shards that do
# consume it run in different jobs on different runners, so it would be a cache
# hit only through BuildBuddy — absent on fork PRs. Discovery does not need it:
# `crates/rue-cli-tests/src/main.rs:3142` returns immediately when the variable
# is unset, and `:2032` is the only other read, inside `staged_program()` on the
# per-case execution path. Dropping it fails closed anyway, because a listing
# that comes back empty is an error rather than an absent duplication.
EXECUTION_ONLY_ENV = frozenset({"RUE_CLI_STAGED_PROGRAMS"})


def listing_env(listing: "Listing") -> dict[str, str]:
    return {
        key: value
        for key, value in listing.env.items()
        if key not in EXECUTION_ONLY_ENV
    }


def materialize(buck: Buck, listings: list["Listing"]) -> dict[str, str]:
    """Build each harness and the `$(location ...)` targets its listing needs."""
    wanted: set[str] = set()
    for listing in listings:
        wanted.add(listing.binary)
        for value in list(listing_env(listing).values()) + listing.args:
            for _, payload in MACRO_RE.findall(value):
                wanted.add(normalize(payload))
    return buck.outputs(sorted(wanted))


def expand(value: str, outputs: dict[str, str]) -> str:
    def replace(match: re.Match[str]) -> str:
        target = normalize(match.group(2))
        path = outputs.get(target)
        if path is None:
            raise RuntimeError(f"no built output for {target}")
        return path

    return MACRO_RE.sub(replace, value)


LIST_LINE_RE = re.compile(r"^(?P<name>.+): (?:test|bench(?:mark)?)$")
# Both harnesses close a listing with a count: rustc's libtest writes
# "856 tests, 0 benchmarks", libtest2-mimic writes "1858 tests". Its presence is
# what separates "this binary listed nothing because it owns nothing" from "this
# binary ignored --list and did something else entirely" — which is not a
# theoretical distinction: `//:spec-traceability` wraps rue-spec with
# `--traceability`, and given `--list` it prints the traceability report and
# exits 0. Scraping that for `NAME: test` finds nothing, and the first revision
# of this gate read the silence as "no tests to compare" while having just run a
# premerge suite for the second time in the same job.
LIST_TRAILER_RE = re.compile(r"^\d+ tests?(?:, \d+ bench(?:mark)?(?:es|s)?)?$")


class Lister:
    """Runs `--list`, once per distinct (binary, args, env), and measures it.

    ADR-0069 accepts this as new required work on the critical path, so the
    cost is reported rather than assumed. The listings are independent
    processes, so they run concurrently: the number worth quoting is the wall
    time a lane pays, not the summed process time.
    """

    def __init__(self, root: Path, jobs: int | None = None) -> None:
        self.root = root
        self.jobs = jobs or min(8, (os.cpu_count() or 2))
        self.cache: dict[tuple, frozenset[str] | RuntimeError] = {}
        self.unlistable: dict[str, str] = {}
        self.lock = threading.Lock()
        self.timings: list[tuple[float, str]] = []
        self.process_seconds = 0.0
        self.wall_seconds = 0.0
        self.invocations = 0

    @staticmethod
    def _key(executable: str, args: list[str], env: dict[str, str]) -> tuple:
        return (executable, tuple(args), tuple(sorted(env.items())))

    def _invoke(self, key: tuple) -> None:
        executable, args, env_items = key
        environment = dict(os.environ)
        environment.update(dict(env_items))
        started = time.monotonic()
        result = subprocess.run(
            [executable, "--list", *args],
            cwd=str(self.root),
            capture_output=True,
            text=True,
            env=environment,
            check=False,
        )
        elapsed = time.monotonic() - started
        if result.returncode != 0:
            value: frozenset[str] | RuntimeError = RuntimeError(
                f"{executable} --list {' '.join(args)} failed "
                f"({result.returncode}): {result.stderr.strip().splitlines()[-1] if result.stderr.strip() else 'no output'}"
            )
        else:
            lines = result.stdout.splitlines()
            if not any(LIST_TRAILER_RE.match(line.strip()) for line in lines):
                value = RuntimeError(
                    f"{executable} --list {' '.join(args)} produced no libtest "
                    "listing (no test-count trailer); the harness ignored --list"
                )
            else:
                value = frozenset(
                    match.group("name")
                    for match in (LIST_LINE_RE.match(line) for line in lines)
                    if match
                )
        with self.lock:
            self.cache[key] = value
            self.timings.append((elapsed, f"{Path(executable).name} {' '.join(args)}"[:96]))
            self.process_seconds += elapsed
            self.invocations += 1

    def prime(self, plans: list[tuple]) -> None:
        """Run every distinct listing once, in parallel."""
        pending = []
        seen: set[tuple] = set()
        for key in plans:
            if key in seen or key in self.cache:
                continue
            seen.add(key)
            pending.append(key)
        if not pending:
            return
        started = time.monotonic()
        with ThreadPoolExecutor(max_workers=self.jobs) as pool:
            list(pool.map(self._invoke, pending))
        self.wall_seconds += time.monotonic() - started

    def _list(self, executable: str, args: list[str], env: dict[str, str]) -> frozenset[str]:
        key = self._key(executable, args, env)
        if key not in self.cache:
            self.prime([key])
        value = self.cache[key]
        if isinstance(value, RuntimeError):
            raise value
        return value

    @staticmethod
    def plans(listing: Listing, outputs: dict[str, str]) -> list[tuple]:
        """Every listing invocation `identities` will need, for `prime`."""
        if listing.target in NOT_LISTABLE:
            return []
        executable = outputs[listing.binary]
        args = [expand(arg, outputs) for arg in listing.args]
        env = {key: expand(value, outputs) for key, value in listing_env(listing).items()}
        keys = [Lister._key(executable, args, env)]
        if listing.has_ignored and "--ignored" not in args:
            keys.append(Lister._key(executable, [*args, "--ignored"], env))
        return keys

    def identities(self, listing: Listing, outputs: dict[str, str]) -> frozenset[str] | None:
        """The tests this target runs, or None when its Rust harness refuses.

        `rue-oracle-diff` and `rue-frontend-diff` are Rust binaries with their
        own argument grammar; they exit non-zero on `--list` at once, costing
        nothing. Returning None is not a quiet downgrade — `collect` requires
        every such target to be declared in `NOT_LISTABLE`, so a binary that
        *stops* listing fails the gate instead of collapsing its tests into one
        opaque unit.
        """
        if listing.target in NOT_LISTABLE:
            # Declared, so not probed. `rue-spec --traceability` is the reason
            # this is a skip rather than a probe-and-tolerate: asking it costs a
            # second execution of a premerge suite, which is the very thing this
            # gate exists to prevent.
            self.unlistable[listing.target] = "declared NOT_LISTABLE; not probed"
            return None
        executable = outputs[listing.binary]
        args = [expand(arg, outputs) for arg in listing.args]
        env = {key: expand(value, outputs) for key, value in listing_env(listing).items()}

        try:
            listed = self._list(executable, args, env)
        except RuntimeError as error:
            self.unlistable[listing.target] = str(error).splitlines()[-1].strip()
            return None
        if listing.has_ignored and "--ignored" not in args:
            # libtest lists ignored tests too, and an ignored test is not
            # scheduled work. `--ignored` inverts the selection, which is
            # exactly how the scaling-matrix canary picks its three rows out of
            # the shared binary — so subtracting that set leaves what runs.
            try:
                listed = listed - self._list(executable, [*args, "--ignored"], env)
            except RuntimeError:
                pass
        return frozenset(f"{listing.namespace_key}\t{name}" for name in listed)


# --------------------------------------------------------------------------
# Lane assembly
# --------------------------------------------------------------------------


def affected_targets(subcommand: str, *args: str, script: Path = AFFECTED_TARGETS) -> list[str]:
    result = subprocess.run(
        ["bash", str(script), subcommand, *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"scripts/affected-targets {subcommand} failed: {result.stderr.strip()}"
        )
    return [normalize(token) for token in result.stdout.split()]


def lane_membership(buck: Buck, script: Path = AFFECTED_TARGETS) -> dict[str, list[str]]:
    """Which targets each lane schedules, derived rather than transcribed."""
    premerge = buck.uquery(PREMERGE_QUERY, "^labels$")
    labels = {target: set(node.get("labels") or []) for target, node in premerge.items()}

    shards = {target for target, tags in labels.items() if CLI_SHARD_LABEL in tags}
    dedicated = {target for target, tags in labels.items() if DEDICATED_LANE_LABEL in tags}
    clippy_owned = {target for target, tags in labels.items() if CLIPPY_LANE_LABEL in tags}

    lanes = {
        "linux-premerge": sorted(set(labels) - shards - dedicated - clippy_owned),
        "platform-corpus": sorted(affected_targets("corpus-targets", script=script)),
        # RUE-1855: these are runnable sh_tests, not representative proxies.
        # Their live inventory is shared with clippy's affected-lane selector.
        "clippy": sorted(affected_targets("lane-targets", "clippy", script=script)),
        "release": sorted(affected_targets("lane-targets", "release", script=script)),
    }
    for lane in ("native-linux-arm64", "native-macos-arm64"):
        # The manifest-driven corpora appear in the native lanes' selection
        # entry because an `only_on` case of theirs can be affected, but the
        # lanes run `scripts/run-native-platform-corpus.sh`'s host-evaluated
        # `only_on` subset, not the corpus. That subset is a property of the
        # runner's own host, so a linux-x64 gate cannot enumerate it; the
        # RUE-1161 platform responsibility gate covers that surface instead.
        lanes[lane] = sorted(
            target
            for target in affected_targets("lane-targets", lane, script=script)
            if target not in dedicated
        )

    for lane in ("native-linux-arm64", "native-macos-arm64"):
        lanes[lane].extend(
            f"{NATIVE_CLI_ALIAS} {' '.join(invocation)}"
            for invocation in NATIVE_CLI_INVOCATIONS
        )

    known = set(affected_targets("lanes", script=script)) | {"linux-premerge", "platform-corpus"}
    unclassified = known - set(lanes) - SELECTION_PROXY_LANES
    if unclassified:
        raise RuntimeError(
            "scripts/affected-targets knows lanes this gate does not classify: "
            + ", ".join(sorted(unclassified))
            + ". Add it to LANE_PLATFORMS if it schedules Buck test targets, or "
            "to SELECTION_PROXY_LANES if its targets only represent it for "
            "selection."
        )
    return lanes


def owner_of(target: str, shards: set[str]) -> str:
    """The target a duplicate set is attributed to.

    A CLI shard is a scheduling alternative for `//:cli-tests`, and which shard
    holds a case is decided by measured weights that move. Attributing to the
    corpus keeps a written allowance from expiring because a case changed
    shards. `duplicate_sets` undoes the fold when the duplication is between
    two members of the fold group, so this cannot hide a cross-shard overlap.

    The three `scripts/rue cli <filter>` workflow steps attribute to the alias
    they all run, for the same reason: one fact, one entry.
    """
    if target in shards:
        return re.sub(r"-shard-\d+$", "", target)
    if target.startswith(f"{NATIVE_CLI_ALIAS} "):
        return NATIVE_CLI_ALIAS
    return target


def workflow_step_listings(buck: Buck) -> dict[str, Listing]:
    """Listings for the native lanes' three `scripts/rue cli` selections.

    RUE-1265's first revision modelled only Buck test targets, and these three
    steps are not one. The ABI selection runs the historical broad `abi`
    substring filter but exactly skips the unrelated differential-opt case;
    that preserves all 61 native ABI assertions instead of narrowing to the 16
    cases in the `cli.abi` section. The alias is a real graph node carrying its
    own env, so only these arguments are declared here. They also live in
    `ci.yml`, and `scripts/validate-ci-gate.py` imports them from here so the
    two cannot drift.

    It matters because the alias leaves both `RUE_CLI_CASE_TIER` and
    `RUE_PLATFORM_CASE_SELECTION` unset — every tier, every case, not the
    `only_on` subset — and `cases/abi.toml` declares no `only_on` at all. Those
    cases run on linux-x64 in the CLI shards and again on both native lanes,
    and unlike the host-evaluated native corpus they are fully enumerable from
    a linux-x64 gate. Omitting them let the invariant overclaim.
    """
    node = buck.uquery(f"set({NATIVE_CLI_ALIAS})", "^(buck.type|exe|env)$").get(
        NATIVE_CLI_ALIAS
    )
    if node is None or node.get("buck.type") != "command_alias":
        raise RuntimeError(
            f"{NATIVE_CLI_ALIAS} is not a command_alias; the native lanes' "
            "`scripts/rue cli` steps can no longer be enumerated"
        )
    binary = normalize(node["exe"])
    env = dict(node.get("env") or {})
    # The same namespace the corpus suites get, because it is the same binary;
    # otherwise these cases could never collide with the CLI shards' and the
    # overlap this exists to find would be invisible.
    harness = buck.uquery(f"set({binary})", ATTRIBUTES).get(binary, {})
    namespace = namespace_key(binary, harness)
    return {
        f"{NATIVE_CLI_ALIAS} {' '.join(invocation)}": Listing(
            target=f"{NATIVE_CLI_ALIAS} {' '.join(invocation)}",
            binary=binary,
            args=list(invocation),
            env=env,
            namespace_key=namespace,
        )
        for invocation in NATIVE_CLI_INVOCATIONS
    }


def lane_units(members: list[str], expansion: dict[str, list[str]]) -> list[str]:
    """The leaf targets one lane actually executes, each exactly once.

    A `test_suite` is not a unit of work: buck2 runs its members, and a member
    named both directly and through a suite still runs once. Counting the name
    twice would be this gate inventing a duplication no runner performs.
    """
    return sorted({unit for member in members for unit in expansion.get(member, [member])})


def collect(buck: Buck, script: Path = AFFECTED_TARGETS) -> tuple[list[Scheduled], Lister, Inventory]:
    lanes = lane_membership(buck, script)
    # A workflow step is written `<target> <filter>` and is not a Buck target,
    # so it never reaches the graph queries; `workflow_step_listings` derives it
    # from the alias instead.
    scheduled_targets = {
        target
        for members in lanes.values()
        for target in members
        if " " not in target
    }
    inventory = resolve_inventory(buck, scheduled_targets)
    if inventory.unresolved:
        raise RuntimeError(
            "could not resolve what these targets execute, so their contents "
            "would be invisible rather than compared: "
            + "; ".join(
                f"{target} ({reason})"
                for target, reason in sorted(inventory.unresolved.items())
            )
        )
    inventory.listings.update(workflow_step_listings(buck))
    outputs = materialize(buck, list(inventory.listings.values()))
    lister = Lister(buck.root)

    shard_nodes = buck.uquery(
        f"attrfilter(labels, '{CLI_SHARD_LABEL}', set(//... toolchains//...))", "^labels$"
    )
    shards = set(shard_nodes)

    # One parallel pass over every distinct listing, so the cost this gate adds
    # to the critical path is bounded by the slowest binary rather than by the
    # sum of all of them.
    lister.prime(
        [
            key
            for listing in inventory.listings.values()
            for key in Lister.plans(listing, outputs)
        ]
    )

    schedule: list[Scheduled] = []
    empty: list[str] = []
    for lane, members in lanes.items():
        platform = LANE_PLATFORMS[lane]
        for unit in lane_units(members, inventory.units):
            listing = inventory.listings.get(unit)
            identities = lister.identities(listing, outputs) if listing else None
            if identities is None:
                identities = frozenset({f"target:{unit}\t<whole target>"})
            if not identities:
                # A well-formed listing of zero tests: the binary exists, runs,
                # and owns nothing to compare.
                # It is still a unit of work, so it counts as one — dropping it
                # from the schedule, as the first revision did, is how two whole
                # suites left the comparison without anything saying so.
                empty.append(unit)
                identities = frozenset({f"target:{unit}\t<no tests>"})
            schedule.append(
                Scheduled(
                    platform=platform,
                    lane=lane,
                    target=unit,
                    owner=owner_of(unit, shards),
                    identities=identities,
                )
            )

    inventory.empty.update(empty)

    undeclared = sorted(set(lister.unlistable) - set(NOT_LISTABLE))
    if undeclared:
        raise RuntimeError(
            "a Rust harness stopped answering --list, which silently collapses "
            "every test it owns into one opaque unit: "
            + "; ".join(f"{target} ({lister.unlistable[target]})" for target in undeclared)
            + ". Fix the harness, or declare it in NOT_LISTABLE with a reason "
            "stating what the opacity hides."
        )
    recovered = sorted(
        (set(NOT_LISTABLE) - set(lister.unlistable)) & set(inventory.listings)
    )
    if recovered:
        raise RuntimeError(
            "these targets are declared NOT_LISTABLE but now enumerate their "
            "tests; remove the declaration so the gate scores them: "
            + ", ".join(recovered)
        )
    return schedule, lister, inventory


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--buck", type=Path, default=DEFAULT_BUCK)
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument(
        "--affected-targets",
        type=Path,
        default=AFFECTED_TARGETS,
        help="lane and corpus inventory script (the determinator's own list)",
    )
    parser.add_argument(
        "--report",
        action="store_true",
        help="print every lane's identity count even when the gate passes",
    )
    args = parser.parse_args()

    buck = Buck(args.buck, args.repo_root)
    started = time.monotonic()
    try:
        schedule, lister, inventory = collect(buck, args.affected_targets)
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    wall = time.monotonic() - started

    duplicates = duplicate_sets(schedule)
    errors = review(duplicates)

    if args.report:
        for entry in sorted(schedule, key=lambda item: (item.platform, item.lane, item.target)):
            print(f"{entry.platform:12} {entry.lane:20} {entry.target} — {len(entry.identities)} test(s)")
        for target, reason in sorted(inventory.not_libtest.items()):
            print(f"not libtest: {target} — {reason}")
        print("slowest listings:")
        for elapsed, description in sorted(lister.timings, reverse=True)[:5]:
            print(f"  {elapsed:6.2f}s  {description}")

    # Unconditional, not behind --report. These are the targets whose contents
    # the comparison cannot see; a reader of a passing CI log has to be able to
    # tell how much of the graph the verdict actually covers, and the step in
    # ci.yml does not pass --report.
    print(
        f"opaque: {len(inventory.opaque)} unit(s) carry no enumerable inventory "
        f"({len(inventory.not_libtest)} non-libtest harness, "
        f"{len(lister.unlistable)} declared NOT_LISTABLE, "
        f"{len(inventory.empty)} listing zero tests)"
    )
    for target in sorted(lister.unlistable):
        print(f"  not listable: {target} — {NOT_LISTABLE[target].split('.')[0]}.")

    total = sum(len(entry.identities) for entry in schedule)
    print(
        f"scheduled {total} test executions across {len(schedule)} target/lane pairs; "
        f"{lister.invocations} --list invocations cost {lister.wall_seconds:.2f}s wall "
        f"({lister.process_seconds:.2f}s of process time across {lister.jobs} workers); "
        f"{wall:.2f}s including graph queries and materialization"
    )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    allowed = sum(len(entry.identities) for entry in duplicates)
    print(
        f"no test executes twice per platform per run: {len(duplicates)} declared "
        f"duplicate set(s) covering {allowed} test(s), none undeclared"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
