#!/bin/sh
set -eu

# Exercise load_cases through the real CLI corpus harness. The malformed
# markers must be rejected before libtest2 receives any trials; the canonical
# marker and a valid known_bug_on platform must still load; scoped xfail cases
# must preserve their expected-failure behavior.

: "${RUE_CLI_HARNESS:?RUE_CLI_HARNESS must point to the CLI harness}"
: "${RUE_BINARY:?RUE_BINARY must point to the Rue compiler}"
: "${RUE_EXAMPLES_DIR:?RUE_EXAMPLES_DIR must point to the examples directory}"
: "${RUE_STD_DIR:?RUE_STD_DIR must point to the std directory}"
: "${RUE_REPO_DIR:?RUE_REPO_DIR must point to the repository root}"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/rue-known-bug-marker.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

host_target() {
    case "$(uname -s):$(uname -m)" in
        Linux:x86_64) printf '%s\n' 'x86-64-linux' ;;
        Linux:aarch64|Linux:arm64) printf '%s\n' 'aarch64-linux' ;;
        Darwin:x86_64) printf '%s\n' 'x86-64-macos' ;;
        Darwin:arm64|Darwin:aarch64) printf '%s\n' 'aarch64-macos' ;;
        *)
            echo "unsupported host: $(uname -s) $(uname -m)" >&2
            exit 1
            ;;
    esac
}

foreign_target() {
    case "$(host_target)" in
        x86-64-linux) printf '%s\n' 'aarch64-linux' ;;
        aarch64-linux) printf '%s\n' 'x86-64-linux' ;;
        x86-64-macos) printf '%s\n' 'aarch64-macos' ;;
        aarch64-macos) printf '%s\n' 'x86-64-macos' ;;
    esac
}

write_case() {
    marker="$1"
    known_bug_on="${2:-}"
    known_bug_on_line=""
    if [ -n "$known_bug_on" ]; then
        known_bug_on_line="known_bug_on = [\"$known_bug_on\"]"
    fi
    cat > "$work_dir/markers.toml" <<EOF
[timeout_profile.ordinary]
compile_hang_timeout_ms = 1000
runtime_hang_timeout_ms = 1000
[timeout_profile.slow]
compile_hang_timeout_ms = 2000
runtime_hang_timeout_ms = 2000
[timeout_profile.stress]
compile_hang_timeout_ms = 3000
runtime_hang_timeout_ms = 3000

[section]
id = "known_bug.validation"
name = "Known-bug marker validation"

[[case]]
name = "marker"
skip = true
known_bug = "$marker"
$known_bug_on_line
EOF
}

write_failure_case() {
    known_bug_on="$1"
    cat > "$work_dir/markers.toml" <<EOF
[timeout_profile.ordinary]
compile_hang_timeout_ms = 1000
runtime_hang_timeout_ms = 1000
[timeout_profile.slow]
compile_hang_timeout_ms = 2000
runtime_hang_timeout_ms = 2000
[timeout_profile.stress]
compile_hang_timeout_ms = 3000
runtime_hang_timeout_ms = 3000

[section]
id = "known_bug.validation"
name = "Known-bug marker validation"

[[case]]
name = "ordinary_failure"
known_bug = "RUE-123"
known_bug_on = ["$known_bug_on"]
files = [{ path = "main.rue", source = "fn main() -> i32 { 0 }" }]
exit_code = 1
EOF
}

check_rejected() {
    marker="$1"
    expected="${2:-$marker}"
    write_case "$marker"
    set +e
    output="$({
        RUE_CLI_CASES="$work_dir" \
        RUE_CLI_CASE_TIER=premerge \
        "$RUE_CLI_HARNESS" --exact known_bug.validation::marker --quiet
    } 2>&1)"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "invalid known_bug marker unexpectedly loaded: $marker" >&2
        echo "$output" >&2
        exit 1
    fi
    printf '%s\n' "$output" | grep -F "markers.toml" >/dev/null
    printf '%s\n' "$output" | grep -F "case 'marker'" >/dev/null
    printf '%s\n' "$output" | grep -F "invalid known_bug marker \"$expected\"" >/dev/null
}

check_known_bug_on_rejected() {
    write_case "RUE-123" "x86_64-linux"
    set +e
    output="$({
        RUE_CLI_CASES="$work_dir" \
        RUE_CLI_CASE_TIER=premerge \
        "$RUE_CLI_HARNESS" --exact known_bug.validation::marker --quiet
    } 2>&1)"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "invalid known_bug_on platform unexpectedly loaded" >&2
        echo "$output" >&2
        exit 1
    fi
    printf '%s\n' "$output" | grep -F "markers.toml" >/dev/null
    printf '%s\n' "$output" | grep -F "case 'marker'" >/dev/null
    printf '%s\n' "$output" | grep -F "unknown known_bug_on platform(s): x86_64-linux" >/dev/null
}

check_host_scoped_xfail() {
    write_failure_case "$(host_target)"
    output="$({
        RUE_CLI_CASES="$work_dir" \
        RUE_CLI_CASE_TIER=premerge \
        "$RUE_CLI_HARNESS" --exact known_bug.validation::ordinary_failure
    } 2>&1)"
    printf '%s\n' "$output" | grep -F "ignored" >/dev/null
}

check_foreign_scoped_xfail_does_not_apply() {
    write_failure_case "$(foreign_target)"
    set +e
    output="$({
        RUE_CLI_CASES="$work_dir" \
        RUE_CLI_CASE_TIER=premerge \
        "$RUE_CLI_HARNESS" --exact known_bug.validation::ordinary_failure
    } 2>&1)"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "known_bug_on incorrectly applied to a different valid platform" >&2
        echo "$output" >&2
        exit 1
    fi
}

for marker in "" "TYPO" "BUG-1" "RUE-" "RUE-0" "RUE-00" "RUE-01" "RUE-+1" "RUE-1x" "RUE-1 "; do
    check_rejected "$marker"
done

check_known_bug_on_rejected

check_host_scoped_xfail
check_foreign_scoped_xfail_does_not_apply

echo "known_bug metadata validation: malformed markers rejected; scoped xfail behavior preserved"
