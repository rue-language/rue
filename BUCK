# Repo-root test-suite targets (RUE-144 / RUE-132).
#
# Each sh_test ties a test-harness binary to the rue compiler and the on-disk
# inputs the suite actually reads (cases/, std/), so Buck owns the binary
# handoff and caches each suite against its real inputs:
#
#   buck2 test //...        # runs unit tests + spec + UI + CLI suites
#
# An edit under crates/rue-spec/cases/ re-runs only the spec suite; an edit
# under std/ re-runs the CLI suite (std/ MUST be a declared input here or the
# CLI suite would get false cache hits on std library changes).
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

# The std library sources are runtime inputs to the CLI integration tests
# (compiled programs `@import` them via ${REAL_STD} / RUE_STD_DIR).
filegroup(
    name = "std",
    srcs = glob(["std/**"]),
)

sh_test(
    name = "spec-tests",
    test = "//crates/rue-spec:rue-spec",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_SPEC_CASES": "$(location //crates/rue-spec:cases)/cases",
    },
)

sh_test(
    name = "ui-tests",
    test = "//crates/rue-ui-tests:rue-ui-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_UI_CASES": "$(location //crates/rue-ui-tests:cases)/cases",
    },
)

sh_test(
    name = "cli-tests",
    test = "//crates/rue-cli-tests:rue-cli-tests",
    args = ["--quiet"],
    env = {
        "RUE_BINARY": "$(exe_target //crates/rue:rue)",
        "RUE_CLI_CASES": "$(location //crates/rue-cli-tests:cases)/cases",
        "RUE_STD_DIR": "$(location :std)/std",
    },
)
