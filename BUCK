# Repo-root test-suite targets (RUE-144 / RUE-132).
#
# Each suite ties a test-harness binary to the rue compiler and the on-disk
# inputs it actually reads (cases/, std/, docs/), so Buck owns the binary
# handoff and keys each suite on its real inputs:
#
#   buck2 test //...        # runs unit tests + spec/UI/CLI suites + repo gates
#
# An edit under crates/rue-spec/cases/ re-runs only the spec suite; an edit
# under std/ re-runs the CLI and spec suites (std/ MUST be a declared input here
# or either suite could get false cache hits on standard-library changes).
#
# RUE-1118: the heavy corpora run through `cached_corpus_suite` rather than a
# bare sh_test. buck2 re-executes every test invocation — test executions are
# not actions and never reach the action cache — so a plain sh_test re-ran the
# whole corpus on each merge even when the merge commit's tree was byte-identical
# to the tree the PR run had just validated. The macro moves the harness into a
# cacheable build action and leaves a thin sh_test asserting its stamp, so the
# suite keeps its name, labels, and result line. See corpus.bzl for the input
# contract this makes load-bearing.
#
# Mechanics: the harness binaries already locate everything via env vars
# (rue-test-runner's find_rue_binary / find_dir), and a filegroup's output
# directory is named after the rule and contains its srcs at package-relative
# paths, hence the `$(location ...)/cases` shape. In an sh_test those macros
# expand to absolute paths and the test runs from the project root; in a
# genrule they are relative to the action's working directory, which is why
# cached_corpus_suite routes them through scripts/corpus-action.
#
# These suites live at the repo root rather than in the harness crates' BUCK
# files so that `buck2 test //crates/...` (quick-test.sh, test.sh's filtered
# path) still means "unit tests only".

load(":corpus.bzl", "cached_corpus_suite")

# The two halves of a cached corpus suite: the action wrapper that runs a
# harness and writes its stamp, and the thin check that asserts the stamp.
sh_binary(
    name = "corpus-action",
    main = "scripts/corpus-action",
)

sh_binary(
    name = "corpus-stamp-check",
    main = "scripts/corpus-stamp-check",
    visibility = ["PUBLIC"],
)

# The formatting gate lives in fmt.sh, not here. A `fmt-check` sh_test used to
# sit at this spot, taking its file list from `glob(["crates/**/*.rs"])`. A
# Buck glob does not descend into subpackages, and all 30 crates that own a
# BUCK file are subpackages -- so the list resolved to the single source under
# crates/rue-runtime-asan/ (the one crate without a BUCK file). The gate ran
# rustfmt over 1 file of 281 and reported a pass for the other 280 (RUE-1152).
#
# Restoring cache-aware checking here means enumerating sources per crate,
# which is a real change rather than a glob tweak; until then CI calls
# `./fmt.sh check`, whose `find`-based discovery covers every source and now
# fails rather than exits 0 when it finds none.

# The std library sources are runtime inputs to CLI integration tests and spec
# cases that opt into the real std (compiled programs `@import` them via
# ${REAL_STD}, RUE_STD_DIR, or RUE_REAL_STD_PATH).
filegroup(
    name = "std",
    srcs = glob(["std/**"]),
    visibility = ["PUBLIC"],
)

# The example programs are runtime inputs to the CLI integration tests: the
# suite compiles+runs every examples/*.rue through the real driver (RUE-48),
# so an edit under examples/ MUST re-run the CLI suite (declared here as an
# input, resolved to an absolute path via RUE_EXAMPLES_DIR below).
filegroup(
    name = "examples",
    srcs = glob(["examples/**"]),
)

# Syntax-valid, checked-in Rue programs compared by the independent stage-1
# frontend differential. Keeping the selection explicit excludes intentionally
# malformed UI/spec/CLI fixtures without a filename heuristic.
filegroup(
    name = "frontend-diff-corpus",
    srcs = dict([(path, path) for path in glob([
        "benchmarks/**/*.rue",
        "examples/**/*.rue",
        "reproducibility/**/*.rue",
        "std/**/*.rue",
    ])] + [
        ("benchmarks/scenarios/representative/labels.rue", "//benchmarks/scenarios/representative:labels.rue"),
        ("benchmarks/scenarios/representative/labels_alt.rue", "//benchmarks/scenarios/representative:labels_alt.rue"),
        ("benchmarks/scenarios/representative/main.rue", "//benchmarks/scenarios/representative:main.rue"),
        ("benchmarks/scenarios/representative/model.rue", "//benchmarks/scenarios/representative:model.rue"),
        ("benchmarks/scenarios/representative/report.rue", "//benchmarks/scenarios/representative:report.rue"),
        ("benchmarks/scenarios/representative/std/_std.rue", "//benchmarks/scenarios/representative:std_root.rue"),
        ("benchmarks/scenarios/representative/std/math.rue", "//benchmarks/scenarios/representative:std_math.rue"),
        ("benchmarks/scenarios/representative/worker.rue", "//benchmarks/scenarios/representative:worker.rue"),
    ]),
    visibility = ["PUBLIC"],
)

# Checked-in repo-relative source_path fixtures for the CLI integration tests.
filegroup(
    name = "cli-test-fixtures",
    srcs = glob(["cli-test-fixtures/**"]),
)

# A deliberately adversarial multi-module project used to assert that Rue's
# complete native output is byte-reproducible across relocated source roots and
# scheduling/environment perturbations (RUE-616).
filegroup(
    name = "reproducibility-fixture",
    srcs = glob(["reproducibility/fixture/**"]),
)

# Tutorial markdown is an input to the snippet checker. The checker only
# compiles fences explicitly marked with `rue check` or `rue compile-fail`.
filegroup(
    name = "tutorial",
    srcs = glob(["website/content/tutorial/**"]),
)

filegroup(
    name = "tutorial-snippet-tool-inputs",
    srcs = [
        "scripts/check-tutorial-snippets.py",
        "test.sh",
    ],
)

filegroup(
    name = "spec-docs",
    srcs = glob(["docs/spec/src/**"]),
)

filegroup(
    name = "adr-designs",
    srcs = glob(["docs/designs/**"]),
)

# Required pull-request and merge-group CI must not execute a moving container
# tag. Keep this list explicit so the policy follows branch-protection scope
# rather than accidentally treating an unrelated maintenance workflow as a
# required check.
filegroup(
    name = "required-ci-workflows",
    srcs = [".github/workflows/ci.yml"],
)

benchmark_tool_local_inputs = glob(["benchmarks/**"]) + [
        "scripts/append-benchmark.py",
        "scripts/benchmark_collection.py",
        "scripts/benchmark_manifest.py",
        "scripts/benchmark_scaling.py",
        "scripts/benchmark_annotations.py",
        "scripts/benchmark_evolution.py",
        "scripts/benchmark_history.py",
        "scripts/benchmark_metrics.py",
        "scripts/benchmark_recent.py",
        "scripts/benchmark_scenarios.py",
        "scripts/benchmark_validation.py",
        "scripts/generate-charts.py",
        "scripts/generate-site-status.py",
        "scripts/parser-profile.py",
        "scripts/perf-baseline.py",
        "scripts/scaling_workloads.py",
        "scripts/validate-benchmark.py",
        "website/build.sh",
        "website/css/input.css",
        "website/templates/performance.html",
        "website/templates/index.html",
    ]

filegroup(
    name = "benchmark-tool-inputs",
    srcs = dict([(path, path) for path in benchmark_tool_local_inputs] + [
        ("benchmarks/scenarios/representative/labels.rue", "//benchmarks/scenarios/representative:labels.rue"),
        ("benchmarks/scenarios/representative/labels_alt.rue", "//benchmarks/scenarios/representative:labels_alt.rue"),
        ("benchmarks/scenarios/representative/main.rue", "//benchmarks/scenarios/representative:main.rue"),
        ("benchmarks/scenarios/representative/model.rue", "//benchmarks/scenarios/representative:model.rue"),
        ("benchmarks/scenarios/representative/report.rue", "//benchmarks/scenarios/representative:report.rue"),
        ("benchmarks/scenarios/representative/std/_std.rue", "//benchmarks/scenarios/representative:std_root.rue"),
        ("benchmarks/scenarios/representative/std/math.rue", "//benchmarks/scenarios/representative:std_math.rue"),
        ("benchmarks/scenarios/representative/worker.rue", "//benchmarks/scenarios/representative:worker.rue"),
        ("crates/rue/src/main.rs", "//crates/rue:benchmark-definition"),
        ("crates/rue-compiler-session-bench/src/main.rs", "//crates/rue-compiler-session-bench:benchmark-main-definition"),
        ("crates/rue-compiler-session-bench/src/representative.rs", "//crates/rue-compiler-session-bench:benchmark-representative-definition"),
    ]),
)

cached_corpus_suite(
    name = "spec-tests",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-spec:rue-spec",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REAL_STD_PATH": "$(location :std)/std",
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_REAL_STD_PATH",
        "RUE_SPEC_CASES",
    ],
)

sh_test(
    name = "spec-traceability",
    test = "//crates/rue-spec:rue-spec",
    args = ["--traceability"],
    env = {
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
        "RUE_SPEC_DIR": "$(location :spec-docs)/docs/spec/src",
    },
)

# RUE-1118: RUE_REAL_STD_PATH was missing here. Cases marked `real_std` compile
# against the standard library, and rue-test-runner resolves it through that
# variable with a cwd-relative fallback ("std", "../std", ...). Under the old
# sh_test, which ran from the project root, the fallback silently found the real
# std/ and the suite passed against an input Buck did not know about — the exact
# false-hit hazard this file's header warns about. Declaring it makes std/ a
# tracked input of the UI corpus.
cached_corpus_suite(
    name = "ui-tests",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-ui-tests:rue-ui-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REAL_STD_PATH": "$(location :std)/std",
        "RUE_UI_CASES": "$(location //crates/rue-ui-tests:cases)/cases",
    },
    absolutize = [
        "RUE_BINARY",
        "RUE_REAL_STD_PATH",
        "RUE_UI_CASES",
    ],
)

# Shared verbatim by //:cli-tests and its shards so a slice runs exactly the
# same cases the monolithic target would. Caldera is skipped here because it
# deliberately exceeds the ordinary corpus's aggregate budget; it runs as the
# isolated //:cli-tests-caldera target below.
#
# RUE-1083: Meridian is also skipped. Its cold compile no longer fits a
# reasonable per-case budget on cold CI runners while per-body incremental
# work continues: linux-x64 killed a family case at 120.025s against the
# 120-second long contract, and linux-arm64 killed the automatic example at
# 300.022s even against the widened 300-second extra-long contract. Restore
# both skips when large-program compile time comes back down.
_CLI_TEST_ARGS = [
    "--quiet",
    "--skip", "cli.examples::caldera::main",
    "--skip", "cli.examples::meridian::main",
    "--skip", "cli.examples_meridian",
]

_CLI_TEST_ENV = {
    "RUE_BINARY": "$(exe_target //crates/rue:rue)",
    "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
    "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
    "RUE_REPO_DIR": "$(location :cli-test-fixtures)",
    "RUE_STD_DIR": "$(location :std)/std",
}

# Every _CLI_TEST_ENV entry is a path the harness hands to a compiler spawned
# with a case's temp directory as cwd, so all of them must be absolute (see
# corpus.bzl). The harness's find_dir fallbacks would otherwise resolve against
# the action's working directory and silently miss the real corpus.
_CLI_TEST_ABSOLUTIZE = [
    "RUE_BINARY",
    "RUE_CLI_CASES",
    "RUE_EXAMPLES_DIR",
    "RUE_REPO_DIR",
    "RUE_STD_DIR",
]

# RUE-1083 recalibrated several per-case heavyweight compile budgets upward, so
# the serialized aggregate can exceed a short outer bound. These replace the
# test executor's timeout, which scripts/ci-heavy-suite used to pass and a build
# action does not get; the per-case contracts in execution_contracts.toml remain
# the honest gates. Re-tighten when the per-case budgets come back down.
_CLI_TESTS_TIMEOUT_SECONDS = 1800
_CLI_SHARD_TIMEOUT_SECONDS = 1200

# The full CLI corpus in one invocation: the canonical target that a local
# `./test.sh` full run executes and that the RUE-924 corpus-omission audit
# tracks (REQUIRED_CORPUS_HARNESSES in test.sh).
cached_corpus_suite(
    name = "cli-tests",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-cli-tests:rue-cli-tests",
    args = _CLI_TEST_ARGS,
    env = _CLI_TEST_ENV,
    absolutize = _CLI_TEST_ABSOLUTIZE,
    timeout_seconds = _CLI_TESTS_TIMEOUT_SECONDS,
)

# Required release coverage is deliberately bounded: compile the real driver
# and CLI harness under //platforms:release, then run the representative
# differential-opt corpus through that release-built compiler. The scheduled
# full-release workflow owns exhaustive //... coverage off the PR critical
# path (RUE-1129).
cached_corpus_suite(
    name = "release-smoke",
    harness = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet", "differential_opt"],
    env = _CLI_TEST_ENV,
    absolutize = _CLI_TEST_ABSOLUTIZE,
)

# RUE-1116: parallel CI shards of the CLI corpus. Same harness and declared
# inputs as //:cli-tests, but each sets RUE_CLI_TEST_SHARD=k/N so it runs a
# stable hash-partitioned 1/N slice; the shards' union is the full corpus. They
# carry BOTH labels deliberately:
#   * rue_heavy_suite — scripts/ci-heavy-suite accepts them unchanged, and the
#     broad `buck2 test //... --exclude rue_heavy_suite` pass skips them;
#   * rue_cli_shard — a local `./test.sh` full run runs the monolithic
#     //:cli-tests exactly once instead of re-running every slice (test.sh
#     subtracts rue_cli_shard from its heavy-suite discovery).
# The `platform-corpus` matrix in .github/workflows/ci.yml MUST list all
# CLI_TEST_SHARD_COUNT shards on every platform that runs the CLI corpus;
# //:cli-shard-coverage-validation fails the build if BUCK and the matrix drift.
CLI_TEST_SHARD_COUNT = 4

[
    cached_corpus_suite(
        name = "cli-tests-shard-{}".format(_shard),
        labels = ["rue_heavy_suite", "rue_cli_shard"],
        harness = "//crates/rue-cli-tests:rue-cli-tests",
        args = _CLI_TEST_ARGS,
        env = dict(_CLI_TEST_ENV.items() + [
            ("RUE_CLI_TEST_SHARD", "{}/{}".format(_shard, CLI_TEST_SHARD_COUNT)),
        ]),
        absolutize = _CLI_TEST_ABSOLUTIZE,
        timeout_seconds = _CLI_SHARD_TIMEOUT_SECONDS,
    )
    for _shard in range(CLI_TEST_SHARD_COUNT)
]

# Caldera deliberately pushes a single compiler invocation past the ordinary
# CLI corpus's aggregate budget. Keep it in the required corpus, but isolate it
# so CI can run the stress program in parallel with the ordinary CLI cases.
#
# RUE-1083: Caldera's cold compile still measures far past any reasonable
# required-CI budget: killed at its 300s contract on linux-arm64, and ~31
# minutes end to end (compile + memcheck) with the non-release linux-x64
# compiler in the Valgrind corpus (run 30211132077, 16:48:20 -> 17:19:10
# UTC). This exact target stays a transparent success stub until the
# remaining per-body incremental work brings the stress compile back into a
# reasonable budget. Every other example family runs real cases above.
cached_corpus_suite(
    name = "cli-tests-caldera",
    labels = ["rue_heavy_suite"],
    harness = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet", "caldera"],
    env = dict(_CLI_TEST_ENV.items() + [
        ("RUE_CALDERA_SUCCESS_STUB", "RUE-1083"),
    ]),
    absolutize = _CLI_TEST_ABSOLUTIZE,
)

# RUE-1083: `examples/` is a declared input because this suite now also checks a
# real maintained program (rill) for byte-stable output, not just the
# purpose-built fixture. An edit under examples/ must therefore re-run it.
sh_test(
    # RUE-1118: still a plain sh_test, so it re-runs on every invocation. Its
    # harness is a shell script rather than a target, and unlike the corpus
    # harnesses it reads repository paths directly rather than through declared
    # env inputs — the input contract cached_corpus_suite depends on has to be
    # established before caching this one would be sound rather than a false
    # pass. It is also off the merge queue's critical path.
    name = "reproducible-programs",
    labels = ["rue_heavy_suite"],
    test = "scripts/test-reproducible-output.sh",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
        "RUE_REPRO_FIXTURE": "$(location :reproducibility-fixture)/reproducibility/fixture",
        "RUE_STD_DIR": "$(location :std)/std",
    },
)

# A fixed generated differential corpus in every full test run. The generator
# unit contract pins that seeds 0..63 retain every required fragile source
# shape; this target then compiles and runs those programs through both the
# reference oracle and native codegen. It lives at the root so full/no-argument
# `test.sh` and CI include it while `quick-test.sh` remains unit-only.
sh_test(
    # RUE-1118: still a plain sh_test, for the same reason as
    # //:reproducible-programs above.
    name = "oracle-diff-generated-smoke",
    labels = ["rue_heavy_suite"],
    test = "scripts/oracle-diff-generated-smoke.sh",
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_ORACLE_DIFF_BINARY": "$(exe_target //crates/rue-oracle-diff:rue-oracle-diff)",
    },
    # Preserve enough outer margin for the harness to print all structured
    # findings even if every compiler and native phase consumes its 2s budget.
    test_rule_timeout_ms = 600000,
)

sh_test(
    name = "tutorial-snippet-tests",
    test = "scripts/check-tutorial-snippets.py",
    args = [
        "--quiet",
        "$(location :tutorial)/website/content/tutorial",
    ],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_STD_PATH": "$(location :std)/std",
    },
)

sh_test(
    name = "tutorial-snippet-tool-tests",
    test = "scripts/test-tutorial-snippets.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_TUTORIAL_TEST_ROOT": "$(location :tutorial-snippet-tool-inputs)",
    },
)

sh_test(
    name = "adr-registry-validation",
    test = "scripts/validate-adrs.py",
    args = [
        "--adr-dir",
        "$(location :adr-designs)/docs/designs",
    ],
)

sh_test(
    name = "required-ci-container-pin-validation",
    test = "scripts/validate-required-ci-container-pins.py",
    args = ["$(location :required-ci-workflows)/.github/workflows/ci.yml"],
)

sh_test(
    name = "required-ci-container-pin-tool-tests",
    test = "scripts/test-required-ci-container-pins.py",
    resources = ["scripts/validate-required-ci-container-pins.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

sh_test(
    name = "debug-assert-policy-tool-tests",
    test = "scripts/test-debug-assert-policy.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

sh_test(
    name = "release-configuration-tool-tests",
    test = "scripts/test-release-configuration.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# The root BUCK file, so the CLI-shard coverage gate can read CLI_TEST_SHARD_COUNT
# and the generated shard targets as a declared input.
filegroup(
    name = "root-buck-file",
    srcs = ["BUCK"],
)

# RUE-1116: fail the build if the CLI shard targets in BUCK and the shards
# listed in the required CI matrix drift apart. A shard present in BUCK but
# missing from the matrix would silently drop that fraction of the corpus on CI
# (the RUE-924 false-green failure mode), since nothing else re-runs the slices.
sh_test(
    name = "cli-shard-coverage-validation",
    test = "scripts/validate-cli-shard-coverage.py",
    args = [
        "--buck",
        "$(location :root-buck-file)/BUCK",
        "--workflow",
        "$(location :required-ci-workflows)/.github/workflows/ci.yml",
    ],
)

sh_test(
    name = "cli-shard-coverage-tool-tests",
    test = "scripts/test-cli-shard-coverage.py",
    resources = ["scripts/validate-cli-shard-coverage.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# RUE-1119: pin the deterministic, coverage-deciding logic of the affected-
# corpus selection — the out-of-graph force-full matcher in
# scripts/affected-targets and the fail-open gate in scripts/ci-corpus-selected.
# The test uses local stubs for the BTD/Buck contract, so it proves a selective
# decision without requiring a network download or a real Buck graph.
sh_test(
    name = "affected-targets-tool-tests",
    test = "scripts/test-affected-targets.sh",
    resources = [
        "scripts/affected-targets",
        "scripts/ci-corpus-decision",
        "scripts/ci-corpus-selected",
        "scripts/parse-btd-impacted.py",
    ],
)

sh_test(
    name = "runtime-abi-inventory-validation",
    test = "scripts/validate-runtime-abi-inventory.py",
    args = [
        "--source", "rue-air=$(location //crates/rue-air:runtime-abi-inventory-sources)",
        "--source", "rue-builtins=$(location //crates/rue-builtins:runtime-abi-inventory-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:runtime-abi-inventory-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:runtime-abi-inventory-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:runtime-abi-inventory-sources)",
        "--source", "rue-linker=$(location //crates/rue-linker:runtime-abi-inventory-sources)",
        "--source", "rue-oracle=$(location //crates/rue-oracle:runtime-abi-inventory-sources)",
    ],
)

sh_test(
    name = "runtime-abi-inventory-tool-tests",
    test = "scripts/test-runtime-abi-inventory.py",
    resources = ["scripts/validate-runtime-abi-inventory.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

sh_test(
    name = "type-architecture-inventory-validation",
    test = "scripts/validate-type-architecture.py",
    args = [
        "--source", "rue-air=$(location //crates/rue-air:type-architecture-inventory-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:type-architecture-inventory-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:type-architecture-inventory-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:type-architecture-inventory-sources)",
        "--source", "rue-compiler-session-bench=$(location //crates/rue-compiler-session-bench:type-architecture-inventory-sources)",
        "--source", "rue-oracle=$(location //crates/rue-oracle:type-architecture-inventory-sources)",
    ],
)

sh_test(
    name = "type-architecture-inventory-tool-tests",
    test = "scripts/test-type-architecture.py",
    resources = ["scripts/validate-type-architecture.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

sh_test(
    name = "payload-ownership-inventory-validation",
    test = "scripts/validate-payload-ownership.py",
    args = [
        "--source", "rue-rir=$(location //crates/rue-rir:payload-ownership-inventory-sources)",
        "--source", "rue-air=$(location //crates/rue-air:payload-ownership-inventory-sources)",
        "--source", "rue-cfg=$(location //crates/rue-cfg:payload-ownership-inventory-sources)",
        "--source", "rue-codegen=$(location //crates/rue-codegen:payload-ownership-inventory-sources)",
    ],
)

sh_test(
    name = "payload-ownership-inventory-tool-tests",
    test = "scripts/test-payload-ownership.py",
    resources = ["scripts/validate-payload-ownership.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

sh_test(
    name = "body-analysis-capability-inventory-validation",
    test = "scripts/validate-body-analysis-capabilities.py",
    args = [
        "--source", "rue-air=$(location //crates/rue-air:body-analysis-capability-inventory-sources)",
        "--source", "rue-compiler=$(location //crates/rue-compiler:body-analysis-capability-inventory-sources)",
    ],
)

sh_test(
    name = "body-analysis-capability-inventory-tool-tests",
    test = "scripts/test-body-analysis-capabilities.py",
    resources = ["scripts/validate-body-analysis-capabilities.py"],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

test_suite(
    name = "payload-ownership-compile-fail-tests",
    tests = ["//crates/rue-rir:rue-rir[doc]"],
)

sh_test(
    name = "benchmark-tool-tests",
    test = "scripts/test-benchmark-tools.py",
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUE_BENCHMARK_TEST_ROOT": "$(location :benchmark-tool-inputs)",
        "RUE_DEEP_NESTING_CASE": "$(location //crates/rue-cli-tests:deep-nesting-case)/cases/deep_nesting.toml",
    },
)

# RUE-1091: the value-audit protocol and adversarial fail-closed tests are
# repository gates. Declare every script and checked-in manifest they import so
# Buck cannot reuse a result after the protocol or fixture changes.
sh_test(
    name = "value-audit-tool-tests",
    test = "scripts/test-value-audit.py",
    resources = [
        "scripts/rue-value-audit.py",
        "scripts/perf-baseline.py",
        "benchmarks/manifest.toml",
        "benchmarks/value-audit/manifest.toml",
        "benchmarks/value-audit/README.md",
    ],
    env = {
        "PYTHONDONTWRITEBYTECODE": "1",
    },
)

# The destructive maintenance scripts under test (jj-tidy, worktree-gc). Their
# fail-closed contract is pinned by scripts/test-cleanup-scripts.sh (RUE-567),
# which runs copies of them against fake gh/git/df on PATH — no real repo,
# remote, or disk touched.
filegroup(
    name = "cleanup-script-inputs",
    srcs = [
        "scripts/jj-tidy",
        "scripts/worktree-gc",
    ],
)

sh_test(
    name = "cleanup-script-tests",
    test = "scripts/test-cleanup-scripts.sh",
    env = {
        "RUE_CLEANUP_SCRIPTS_ROOT": "$(location :cleanup-script-inputs)",
    },
)

# The developer wrapper scripts. scripts/test-wrapper-scripts.sh (RUE-537,
# RUE-549, RUE-550, RUE-590) runs copies of them against fake tools — no real
# build — to pin that resolver failures are surfaced (not swallowed), that
# run/exec resolve relative paths from the caller's cwd, that filtered CLI
# examples stay repository-anchored across per-case cwd changes, and that the
# sanitizer gives examples the bundled standard library. The filegroup
# materializes these at package-relative paths, matching the layout expected
# under RUE_WRAPPER_ROOT.
filegroup(
    name = "wrapper-script-inputs",
    srcs = [
        "fmt.sh",
        "scripts/ci-heavy-suite",
        "scripts/ci-timed",
        "scripts/check-cache-probe",
        "scripts/rue",
        "scripts/rue-bin",
        "scripts/provision-build-cache",
        "scripts/with-full-suite-lock",
        "scripts/run-sanitizer.sh",
        "test.sh",
    ],
)

sh_test(
    name = "wrapper-script-tests",
    test = "scripts/test-wrapper-scripts.sh",
    env = {
        "RUE_WRAPPER_ROOT": "$(location :wrapper-script-inputs)",
    },
)

# RUE-1118: corpus-action decides whether a corpus suite's result is written to
# the action cache, so its stamp-only-on-success and absolutization contracts
# are pinned independently of any corpus actually running.
filegroup(
    name = "corpus-script-inputs",
    srcs = [
        "scripts/corpus-action",
        "scripts/corpus-stamp-check",
    ],
)

sh_test(
    name = "corpus-action-tests",
    test = "scripts/test-corpus-action.sh",
    env = {
        "RUE_CORPUS_SCRIPTS_ROOT": "$(location :corpus-script-inputs)",
    },
)

filegroup(
    name = "build-sharing-test-inputs",
    srcs = [
        "buck2",
        "buck2-bin",
        "scripts/ci-heavy-suite",
        "scripts/provision-build-cache",
        "scripts/with-full-suite-lock",
        "test.sh",
    ],
)

sh_test(
    name = "build-sharing-tests",
    test = "scripts/test-build-sharing.sh",
    env = {
        "RUE_BUILD_SHARING_ROOT": "$(location :build-sharing-test-inputs)",
    },
)
