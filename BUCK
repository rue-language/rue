# Repo-root test-suite targets (RUE-144 / RUE-132).
#
# Each sh_test ties a test-harness binary to the rue compiler and the on-disk
# inputs the suite actually reads (cases/, std/, docs/), so Buck owns the binary
# handoff and caches each suite against its real inputs:
#
#   buck2 test //...        # runs unit tests + spec/UI/CLI suites + repo gates
#
# An edit under crates/rue-spec/cases/ re-runs only the spec suite; an edit
# under std/ re-runs the CLI and spec suites (std/ MUST be a declared input here
# or either suite could get false cache hits on standard-library changes).
#
# Mechanics: the harness binaries already locate everything via env vars
# (rue-test-runner's find_rue_binary / find_dir), `$(exe_target ...)` /
# `$(location ...)` expand to absolute paths, and sh_test runs from the
# project root — so the harness binary itself can be the `test` command and
# no wrapper script is needed. A filegroup's output directory is named after
# the rule and contains its srcs at package-relative paths, hence the
# `$(location ...)/cases` shape.
#
# These suites live at the repo root rather than in the harness crates' BUCK
# files so that `buck2 test //crates/...` (quick-test.sh, test.sh's filtered
# path) still means "unit tests only".

# Formatting is a first-class repository check. The source files are both
# passed to rustfmt and declared as resources, so Buck invalidates the cached
# result when any Rust source changes. The rustfmt target's RunInfo owns host
# selection and dynamic-library setup; write-mode formatting remains in
# fmt.sh because build/test actions must not mutate the source tree.
_RUST_SOURCES = glob(["crates/**/*.rs"])

sh_test(
    name = "fmt-check",
    test = "toolchains//rust:rustfmt",
    args = [
        "--edition",
        "2024",
        "--check",
    ] + _RUST_SOURCES,
    resources = _RUST_SOURCES,
)

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

sh_test(
    name = "spec-tests",
    labels = ["rue_heavy_suite"],
    test = "//crates/rue-spec:rue-spec",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_REAL_STD_PATH": "$(location :std)/std",
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
    },
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

sh_test(
    name = "ui-tests",
    labels = ["rue_heavy_suite"],
    test = "//crates/rue-ui-tests:rue-ui-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_UI_CASES": "$(location //crates/rue-ui-tests:cases)/cases",
    },
)

# Shared verbatim by //:cli-tests and its shards so a slice runs exactly the
# same cases the monolithic target would. Caldera is skipped here because it
# deliberately exceeds the ordinary corpus's aggregate budget; it runs as the
# isolated //:cli-tests-caldera target below.
_CLI_TEST_ARGS = [
    "--quiet",
    "--skip", "cli.examples::caldera::main",
]

_CLI_TEST_ENV = {
    "RUE_BINARY": "$(exe_target //crates/rue:rue)",
    "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
    "RUE_EXAMPLES_DIR": "$(location :examples)/examples",
    "RUE_REPO_DIR": "$(location :cli-test-fixtures)",
    "RUE_STD_DIR": "$(location :std)/std",
}

# The full CLI corpus in one invocation: the canonical target that a local
# `./test.sh` full run executes and that the RUE-924 corpus-omission audit
# tracks (REQUIRED_CORPUS_HARNESSES in test.sh).
sh_test(
    name = "cli-tests",
    labels = ["rue_heavy_suite"],
    test = "//crates/rue-cli-tests:rue-cli-tests",
    args = _CLI_TEST_ARGS,
    env = _CLI_TEST_ENV,
)

# Required release coverage is deliberately bounded: compile the real driver
# and CLI harness under //platforms:release, then run the representative
# differential-opt corpus through that release-built compiler. The scheduled
# full-release workflow owns exhaustive //... coverage off the PR critical
# path (RUE-1129).
sh_test(
    name = "release-smoke",
    test = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet", "differential_opt"],
    env = _CLI_TEST_ENV,
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
    sh_test(
        name = "cli-tests-shard-{}".format(_shard),
        labels = ["rue_heavy_suite", "rue_cli_shard"],
        test = "//crates/rue-cli-tests:rue-cli-tests",
        args = _CLI_TEST_ARGS,
        env = dict(_CLI_TEST_ENV.items() + [
            ("RUE_CLI_TEST_SHARD", "{}/{}".format(_shard, CLI_TEST_SHARD_COUNT)),
        ]),
    )
    for _shard in range(CLI_TEST_SHARD_COUNT)
]

# Caldera deliberately pushes a single compiler invocation past the ordinary
# CLI corpus's aggregate budget. Keep it in the required corpus, but isolate it
# so CI can run the stress program in parallel with the ordinary CLI cases.
sh_test(
    name = "cli-tests-caldera",
    labels = ["rue_heavy_suite"],
    test = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet", "caldera"],
    env = _CLI_TEST_ENV,
)

# RUE-1083: `examples/` is a declared input because this suite now also checks a
# real maintained program (rill) for byte-stable output, not just the
# purpose-built fixture. An edit under examples/ must therefore re-run it.
sh_test(
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
