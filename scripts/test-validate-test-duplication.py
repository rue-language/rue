#!/usr/bin/env python3
"""Focused tests for the CI duplication gate (RUE-1265, ADR-0069 §2).

The fixture that matters is `RUE_1262_SHAPE`: a target whose tests are a
strict superset of another's, in the same lane, on the same platform. That is
the defect this gate exists for, and it is reproduced here as data so the gate
is proven to fail on it rather than asserted to.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("validate-test-duplication.py")
SPEC = importlib.util.spec_from_file_location("validate_test_duplication", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GATE = importlib.util.module_from_spec(SPEC)
sys.modules["validate_test_duplication"] = GATE
SPEC.loader.exec_module(GATE)

COMPILER = "//crates/rue-compiler:rue-compiler-test"
SCALING = "//crates/rue-compiler:scaling-matrix-test"
# One namespace, because the pre-RUE-1262 scaling-matrix target compiled the
# same crate sources under a different `--cfg`.
CRATE = "srcs:0123456789ab"


def scheduled(platform, lane, target, names, namespace=CRATE, owner=None):
    return GATE.Scheduled(
        platform=platform,
        lane=lane,
        target=target,
        owner=owner or target,
        identities=frozenset(f"{namespace}\t{name}" for name in names),
    )


SHARED = [f"pipeline_tests::tests::case_{index}" for index in range(813)]

# The RUE-1262 defect, as data: 816 tests against 813, all 813 shared, both in
# the premerge lane on linux-x64.
RUE_1262_SHAPE = [
    scheduled("linux-x64", "linux-premerge", COMPILER, SHARED),
    scheduled(
        "linux-x64",
        "linux-premerge",
        SCALING,
        SHARED + ["scaling_harness::scaling_matrix_identity_invariant"],
    ),
]


class DuplicateDetectionTests(unittest.TestCase):
    def test_superset_target_in_one_lane_is_a_duplicate_set(self):
        duplicates = GATE.duplicate_sets(RUE_1262_SHAPE)
        self.assertEqual(len(duplicates), 1)
        duplicate = duplicates[0]
        self.assertEqual(duplicate.targets, (COMPILER, SCALING))
        self.assertEqual(duplicate.platforms, ("linux-x64",))
        self.assertEqual(len(duplicate.identities), 813)
        # The three tests the superset uniquely owns are not duplicates.
        self.assertNotIn(
            f"{CRATE}\tscaling_harness::scaling_matrix_identity_invariant",
            duplicate.identities,
        )

    def test_the_gate_fails_on_that_fixture_and_names_both_targets(self):
        errors = GATE.review(GATE.duplicate_sets(RUE_1262_SHAPE), allowances=())
        self.assertEqual(len(errors), 1)
        self.assertIn("undeclared duplication", errors[0])
        self.assertIn(COMPILER, errors[0])
        self.assertIn(SCALING, errors[0])
        # A reader must be able to act without re-deriving the overlap: which
        # lane scheduled each copy, and an example of what overlaps.
        self.assertIn("linux-premerge", errors[0])
        self.assertIn("pipeline_tests::tests::case_0", errors[0])

    def test_disjoint_targets_are_not_duplicates(self):
        schedule = [
            scheduled("linux-x64", "linux-premerge", COMPILER, SHARED),
            scheduled(
                "linux-x64",
                "linux-premerge",
                SCALING,
                ["scaling_harness::scaling_matrix_identity_invariant"],
            ),
        ]
        self.assertEqual(GATE.duplicate_sets(schedule), [])
        self.assertEqual(GATE.review([], allowances=()), [])

    def test_same_test_name_in_two_crates_is_not_a_duplicate(self):
        # `tests::empty_program` is an ordinary name; two crates owning one is
        # not a duplication, so identities are namespaced by compiled sources.
        schedule = [
            scheduled(
                "linux-x64", "linux-premerge", "//crates/rue-lexer:rue-lexer-test",
                ["tests::empty_program"], namespace="srcs:aaaaaaaaaaaa",
            ),
            scheduled(
                "linux-x64", "linux-premerge", "//crates/rue-parser:rue-parser-test",
                ["tests::empty_program"], namespace="srcs:bbbbbbbbbbbb",
            ),
        ]
        self.assertEqual(GATE.duplicate_sets(schedule), [])

    def test_cross_platform_repetition_is_reported_as_one_set(self):
        schedule = [
            scheduled(platform, lane, COMPILER, SHARED)
            for platform, lane in (
                ("linux-x64", "linux-premerge"),
                ("linux-arm64", "native-linux-arm64"),
                ("macos-arm64", "native-macos-arm64"),
            )
        ]
        duplicates = GATE.duplicate_sets(schedule)
        self.assertEqual(len(duplicates), 1)
        self.assertEqual(duplicates[0].targets, (COMPILER,))
        self.assertEqual(
            duplicates[0].platforms, ("linux-arm64", "linux-x64", "macos-arm64")
        )


class AllowanceTests(unittest.TestCase):
    def allowance(self, targets, platforms=("linux-x64",), kind="between-targets"):
        return GATE.Allowance(
            targets=tuple(targets),
            platforms=tuple(platforms),
            kind=kind,
            reason="under test",
        )

    def test_declared_allowance_suppresses_the_failure(self):
        errors = GATE.review(
            GATE.duplicate_sets(RUE_1262_SHAPE),
            allowances=(self.allowance([COMPILER, SCALING]),),
        )
        self.assertEqual(errors, [])

    def test_allowance_for_other_platforms_does_not_apply(self):
        errors = GATE.review(
            GATE.duplicate_sets(RUE_1262_SHAPE),
            allowances=(self.allowance([COMPILER, SCALING], ("linux-arm64",)),),
        )
        self.assertTrue(any("undeclared duplication" in error for error in errors), errors)

    def test_allowance_must_name_every_target_involved(self):
        # Naming only one side is exactly the review that would have missed the
        # superset, so a partial allowance must not cover it.
        errors = GATE.review(
            GATE.duplicate_sets(RUE_1262_SHAPE),
            allowances=(self.allowance([COMPILER]),),
        )
        self.assertTrue(any("undeclared duplication" in error for error in errors), errors)

    def test_a_superset_allowance_does_not_absorb_a_smaller_overlap(self):
        # The defect an adversarial review found: subset matching let a
        # `between-targets` entry vouch for any subset of its own targets, so
        # an overlap between two shards — which attributes to the single owner
        # //:cli-tests — slid under the two-target release-smoke entry.
        schedule = [
            scheduled("linux-x64", "platform-corpus", "//:cli-tests-shard-0",
                      SHARED, owner="//:cli-tests"),
            scheduled("linux-x64", "platform-corpus", "//:cli-tests-shard-1",
                      SHARED, owner="//:cli-tests"),
        ]
        duplicates = GATE.duplicate_sets(schedule)
        # The fold onto //:cli-tests is undone, because undoing it is the only
        # way the report can name what actually overlaps.
        self.assertEqual(
            duplicates[0].targets, ("//:cli-tests-shard-0", "//:cli-tests-shard-1")
        )
        errors = GATE.review(
            duplicates,
            allowances=(self.allowance(["//:cli-tests", "//:release-smoke"]),),
        )
        self.assertTrue(any("undeclared duplication" in error for error in errors), errors)

    def test_a_roster_does_not_cover_an_overlap_between_its_own_members(self):
        # The same defect from the other side: eight targets that each repeat
        # across three platforms must not license a NEW overlap between two of
        # them, which is the RUE-1262 shape reproduced inside an allowance.
        platforms = ("linux-arm64", "linux-x64", "macos-arm64")
        roster = self.allowance(["//a:one-test", "//b:two-test"], platforms, "per-target")
        overlap = [
            scheduled(platform, lane, target, ["tests::x"])
            for target, (platform, lane) in zip(
                ("//a:one-test", "//b:two-test"),
                (("linux-x64", "premerge"), ("linux-x64", "premerge")),
            )
        ]
        overlap += [
            scheduled(platform, lane, "//a:one-test", ["tests::x"])
            for platform, lane in (("linux-arm64", "n-arm"), ("macos-arm64", "n-mac"))
        ]
        errors = GATE.review(GATE.duplicate_sets(overlap), allowances=(roster,))
        self.assertTrue(any("undeclared duplication" in error for error in errors), errors)

    def test_one_roster_entry_covers_several_single_target_repetitions(self):
        platforms = ("linux-arm64", "linux-x64", "macos-arm64")
        schedule = []
        for index, target in enumerate(("//a:one-test", "//b:two-test")):
            for platform, lane in zip(platforms, ("n-arm", "premerge", "n-mac")):
                schedule.append(
                    scheduled(
                        platform, lane, target, ["tests::x"],
                        namespace=f"srcs:{index}0000000000",
                    )
                )
        duplicates = GATE.duplicate_sets(schedule)
        self.assertEqual(len(duplicates), 2)
        self.assertEqual(
            GATE.review(
                duplicates,
                allowances=(
                    self.allowance(
                        ["//a:one-test", "//b:two-test"], platforms, "per-target"
                    ),
                ),
            ),
            [],
        )

    def test_a_roster_goes_stale_per_target_not_per_entry(self):
        # Seven of eight targets can stop running while the eighth keeps the
        # entry "matched", leaving seven written reasons vouching for nothing.
        platforms = ("linux-arm64", "linux-x64", "macos-arm64")
        alive = [
            scheduled(platform, lane, "//a:one-test", ["tests::x"])
            for platform, lane in zip(platforms, ("n-arm", "premerge", "n-mac"))
        ]
        errors = GATE.review(
            GATE.duplicate_sets(alive),
            allowances=(
                self.allowance(
                    ["//a:one-test", "//b:gone-test"], platforms, "per-target"
                ),
            ),
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("stale allowance", errors[0])
        self.assertIn("//b:gone-test", errors[0])

    def test_allowance_matching_nothing_is_an_error(self):
        errors = GATE.review([], allowances=(self.allowance([COMPILER, SCALING]),))
        self.assertEqual(len(errors), 1)
        self.assertIn("stale allowance", errors[0])

    def test_the_declared_ledger_carries_a_reason_for_every_entry(self):
        for allowance in GATE.ALLOWANCES:
            self.assertTrue(allowance.targets, allowance)
            self.assertTrue(allowance.platforms, allowance)
            self.assertIn(allowance.kind, ("per-target", "between-targets"), allowance)
            # An allowance is a written decision, not a suppression switch.
            self.assertGreater(len(allowance.reason), 80, allowance.targets)
            self.assertEqual(
                tuple(sorted(allowance.platforms)), allowance.platforms, allowance
            )

    def test_every_not_listable_entry_says_what_its_opacity_hides(self):
        # A NOT_LISTABLE entry removes a target from the comparison entirely,
        # which is a larger claim than an allowance makes. The reason has to
        # carry it.
        for target, reason in GATE.NOT_LISTABLE.items():
            self.assertTrue(target.startswith("//"), target)
            self.assertGreater(len(reason), 120, target)


class InventoryTests(unittest.TestCase):
    def test_shards_are_attributed_to_the_corpus_they_slice(self):
        shards = {f"//:cli-tests-shard-{index}" for index in range(4)}
        self.assertEqual(GATE.owner_of("//:cli-tests-shard-2", shards), "//:cli-tests")
        self.assertEqual(GATE.owner_of("//:release-smoke", shards), "//:release-smoke")
        # A target that merely looks sharded but carries no shard label keeps
        # its own name.
        self.assertEqual(GATE.owner_of("//:other-shard-1", shards), "//:other-shard-1")

    def test_a_target_reached_through_a_suite_and_directly_runs_once(self):
        expansion = {
            "//:repository-quality-gates": ["//:adr-registry-validation", "//:spec-traceability"],
            "//:spec-traceability": ["//:spec-traceability"],
        }
        units = GATE.lane_units(
            ["//:repository-quality-gates", "//:spec-traceability"], expansion
        )
        self.assertEqual(units, ["//:adr-registry-validation", "//:spec-traceability"])

    def test_list_output_is_parsed_and_the_trailer_is_not_a_test(self):
        lines = [
            "pipeline_tests::tests::wide_batches: test",
            "bench_module::throughput: bench",
            "",
            "2 tests, 1 benchmarks",
        ]
        names = [
            match.group("name")
            for match in (GATE.LIST_LINE_RE.match(line) for line in lines)
            if match
        ]
        self.assertEqual(names, ["pipeline_tests::tests::wide_batches", "bench_module::throughput"])

    def test_a_wrapper_whose_harness_cannot_be_read_is_loud(self):
        # The one failure this gate cannot absorb: a wrapper that lists 850
        # tests degrading to "one opaque unit" would make its duplication
        # disappear into a pass.
        class PartialBuck:
            root = Path(".")

            def uquery(self, expression, attributes):
                if "//:hidden-harness" in expression:
                    return {}
                return {
                    "//:wrapper-test": {
                        "buck.type": "sh_test",
                        "test": "root//:hidden-harness",
                        "args": [],
                    }
                }

        inventory = GATE.resolve_inventory(PartialBuck(), {"//:wrapper-test"})
        self.assertIn("//:wrapper-test", inventory.unresolved)
        self.assertIn("//:hidden-harness", inventory.unresolved["//:wrapper-test"])

    def test_a_wrapper_around_a_non_test_target_is_ordinary_opaque_work(self):
        # //:adr-registry-validation runs a script through a `test` attribute
        # that is a source path, and the clippy gates wrap a non-test binary.
        # Neither is a resolution failure; each is one unit of work.
        class ScriptBuck:
            root = Path(".")

            def uquery(self, expression, attributes):
                if "//:clippy-gate" in expression:
                    return {"//:clippy-gate": {"buck.type": "sh_binary"}}
                return {
                    "//:script-test": {
                        "buck.type": "sh_test",
                        "test": "root///scripts/validate-adrs.py",
                        "args": [],
                    },
                    "//:clippy-test": {
                        "buck.type": "sh_test",
                        "test": "root//:clippy-gate",
                        "args": [],
                    },
                }

        inventory = GATE.resolve_inventory(
            ScriptBuck(), {"//:script-test", "//:clippy-test"}
        )
        self.assertEqual(inventory.unresolved, {})
        self.assertEqual(inventory.opaque, {"//:script-test", "//:clippy-test"})

    def test_a_shell_corpus_harness_is_classified_from_the_graph_not_probed(self):
        # The cheapest possible answer: a `sh_binary` cannot be libtest, and
        # asking one anyway runs its whole suite.
        class ShellCorpusBuck:
            root = Path(".")

            def uquery(self, expression, attributes):
                nodes = {
                    "//:reproducible-programs": {
                        "buck.type": "sh_test",
                        "test": "root//:corpus-stamp-check",
                        "args": ["$(location root//:reproducible-programs-action)"],
                    },
                    "//:reproducible-programs-action": {
                        "buck.type": "_corpus_action",
                        "harness": "root//:reproducible-programs-harness",
                        "harness_args": [],
                        "corpus_env": {},
                    },
                    "//:reproducible-programs-harness": {"buck.type": "sh_binary"},
                    "//:corpus-stamp-check": {"buck.type": "sh_binary"},
                }
                return {
                    label: node
                    for label, node in nodes.items()
                    if label in expression
                }

        inventory = GATE.resolve_inventory(
            ShellCorpusBuck(), {"//:reproducible-programs"}
        )
        self.assertEqual(inventory.listings, {})
        self.assertIn("//:reproducible-programs", inventory.not_libtest)
        self.assertIn("sh_binary", inventory.not_libtest["//:reproducible-programs"])
        self.assertEqual(inventory.unresolved, {})

    def test_a_harness_that_ignores_list_is_not_read_as_an_empty_suite(self):
        # The defect an adversarial review found: `scripts/test-reproducible-output.sh`
        # and `scripts/oracle-diff-generated-smoke.sh` do no argument handling,
        # so `--list` ran the whole suite, exited 0, and printed nothing this
        # parser matches. Zero identities read as "nothing to compare" while
        # the gate had just executed a premerge suite a second time.
        with tempfile.TemporaryDirectory() as directory:
            harness = Path(directory) / "suite.sh"
            harness.write_text(
                "#!/usr/bin/env bash\n"
                "echo 'running 3 checks'\n"
                "echo 'ok 1 - reproducible'\n"
                "exit 0\n"
            )
            harness.chmod(0o755)
            lister = GATE.Lister(Path(directory))
            listing = GATE.Listing(
                target="//:suite", binary="//:suite", args=[], env={},
                namespace_key=CRATE,
            )
            identities = lister.identities(listing, {"//:suite": str(harness)})
        self.assertIsNone(identities)
        self.assertIn("//:suite", lister.unlistable)
        self.assertIn("no libtest listing", lister.unlistable["//:suite"])

    def test_a_well_formed_empty_listing_is_not_the_same_thing(self):
        with tempfile.TemporaryDirectory() as directory:
            harness = Path(directory) / "empty.sh"
            harness.write_text("#!/usr/bin/env bash\necho\necho '0 tests, 0 benchmarks'\n")
            harness.chmod(0o755)
            lister = GATE.Lister(Path(directory))
            listing = GATE.Listing(
                target="//:empty-test", binary="//:empty-test", args=[], env={},
                namespace_key=CRATE,
            )
            identities = lister.identities(listing, {"//:empty-test": str(harness)})
        self.assertEqual(identities, frozenset())
        self.assertEqual(lister.unlistable, {})

    def test_a_declared_not_listable_target_is_never_invoked(self):
        # Probing `//:spec-traceability` costs a second execution of a premerge
        # suite, which is precisely what this gate exists to stop.
        target = next(iter(GATE.NOT_LISTABLE))
        listing = GATE.Listing(
            target=target, binary="//:harness", args=[], env={}, namespace_key=CRATE
        )
        self.assertEqual(GATE.Lister.plans(listing, {"//:harness": "/does/not/exist"}), [])
        lister = GATE.Lister(Path("."))
        self.assertIsNone(lister.identities(listing, {"//:harness": "/does/not/exist"}))
        self.assertEqual(lister.invocations, 0)

    def test_execution_only_env_is_not_materialized(self):
        # //:cli-staged-programs stages ten rue_program compiles that no
        # listing consults, and the premerge lane never builds it otherwise.
        listing = GATE.Listing(
            target="//:cli-tests-shard-0",
            binary="//crates/rue-cli-tests:rue-cli-tests",
            args=[],
            env={
                "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
                "RUE_CLI_STAGED_PROGRAMS": "$(location //:cli-staged-programs)",
            },
            namespace_key=CRATE,
        )
        self.assertEqual(
            set(GATE.listing_env(listing)), {"RUE_CLI_CASES"}
        )

    def test_location_macros_expand_to_built_outputs(self):
        outputs = {"//crates/rue:rue": "/out/rue", "//:std": "/out/std"}
        self.assertEqual(
            GATE.expand("$(exe_target root//crates/rue:rue)", outputs), "/out/rue"
        )
        self.assertEqual(GATE.expand("$(location //:std)/std", outputs), "/out/std/std")

    def test_an_unbuilt_macro_target_is_an_error_rather_than_a_silent_empty_env(self):
        # A harness handed an empty path finds no cases and lists nothing,
        # which would read as "this target duplicates nothing".
        with self.assertRaises(RuntimeError):
            GATE.expand("$(location //:missing)", {})


class LaneModelTests(unittest.TestCase):
    """The gate must not silently ignore a lane someone adds to CI."""

    def fake_affected_targets(self, directory: Path, lanes: list[str]) -> Path:
        script = directory / "affected-targets"
        script.write_text(
            "#!/usr/bin/env bash\n"
            'case "$1" in\n'
            f'  lanes) printf "%s\\n" {" ".join(lanes)} ;;\n'
            "  corpus-targets) echo //:cli-tests-shard-0 ;;\n"
            "  lane-targets) echo //:release-smoke ;;\n"
            "esac\n"
        )
        return script

    class FakeBuck:
        root = Path(".")

        def uquery(self, expression, attributes):
            return {"//crates/rue-lexer:rue-lexer-test": {"labels": []}}

    def test_an_unclassified_lane_fails_the_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            script = self.fake_affected_targets(
                Path(directory), ["release", "valgrind", "brand-new-lane"]
            )
            with self.assertRaises(RuntimeError) as caught:
                GATE.lane_membership(self.FakeBuck(), script)
        self.assertIn("brand-new-lane", str(caught.exception))

    def test_the_known_lanes_are_classified(self):
        with tempfile.TemporaryDirectory() as directory:
            script = self.fake_affected_targets(
                Path(directory), sorted(set(GATE.LANE_PLATFORMS) | GATE.SELECTION_PROXY_LANES)
            )
            lanes = GATE.lane_membership(self.FakeBuck(), script)
        self.assertEqual(set(lanes), set(GATE.LANE_PLATFORMS))

    def test_every_classified_lane_has_a_platform(self):
        self.assertEqual(
            set(GATE.LANE_PLATFORMS) & GATE.SELECTION_PROXY_LANES, set()
        )

    def test_the_determinator_exposes_the_inventories_this_gate_reads(self):
        for subcommand in ("corpus-targets", "lanes"):
            result = subprocess.run(
                ["bash", str(GATE.AFFECTED_TARGETS), subcommand],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(result.stdout.split(), subcommand)


if __name__ == "__main__":
    unittest.main()
