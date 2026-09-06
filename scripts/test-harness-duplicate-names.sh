#!/bin/sh
set -eu

# Exercise the actual spec, UI, and CLI entry points. The filtered fixtures put
# a passing case on this host and a failing duplicate on another host, proving
# validation happens before platform filtering. The all-selected fixtures put
# both duplicates on this host, proving validation keeps them out of libtest2's
# concurrent scheduler.

: "${RUE_SPEC_HARNESS:?RUE_SPEC_HARNESS must point to the spec harness}"
: "${RUE_UI_HARNESS:?RUE_UI_HARNESS must point to the UI harness}"
: "${RUE_CLI_HARNESS:?RUE_CLI_HARNESS must point to the CLI harness}"
: "${RUE_BINARY:?RUE_BINARY must point to the Rue compiler}"

# The host platform and the platform vocabulary come from the harness under
# test, which reads them from rue-target's one host table; a `uname` ladder here
# would be a second answer that can disagree with the harness this gate drives
# (RUE-1987). RUE_HOST_TARGET / RUE_KNOWN_TARGETS override it for a caller that
# already knows.
host="${RUE_HOST_TARGET:-$("$RUE_CLI_HARNESS" --print-host-target)}"
known_targets="${RUE_KNOWN_TARGETS:-$("$RUE_CLI_HARNESS" --print-known-targets)}"
if [ -z "$host" ] || [ -z "$known_targets" ]; then
    echo "could not resolve the host platform for the duplicate-harness" \
        "regression from $RUE_CLI_HARNESS; set RUE_HOST_TARGET and" \
        "RUE_KNOWN_TARGETS to override" >&2
    exit 1
fi
if ! printf '%s\n' "$known_targets" | grep -F -x -q -e "$host"; then
    echo "harness host '$host' is not one of its own known targets" >&2
    exit 1
fi
# Any valid platform other than this host, so the duplicate is filtered out
# here and validation must still reject it.
foreign="$(printf '%s\n' "$known_targets" | grep -F -v -x -e "$host" | head -n 1)"
if [ -z "$foreign" ]; then
    echo "the harness knows only one platform; cannot build a foreign scope" >&2
    exit 1
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/rue-duplicate-harness.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM
mkdir -p "$work_dir/spec" "$work_dir/ui" "$work_dir/cli" \
    "$work_dir/spec-all" "$work_dir/ui-all" "$work_dir/cli-all" \
    "$work_dir/examples" "$work_dir/std"

write_shared_case() {
    directory="$1"
    cat > "$directory/a.toml" <<EOF
[section]
id = "duplicate.section"
name = "Duplicate"

[[case]]
name = "same"
source = "fn main() -> i32 { 0 }"
exit_code = 0
only_on = ["$host"]
EOF
    cat > "$directory/b.toml" <<EOF
[section]
id = "duplicate.section"
name = "Duplicate"

[[case]]
name = "same"
source = "fn main() -> i32 { 0 }"
exit_code = 1
only_on = ["$foreign"]
EOF
}

write_shared_case "$work_dir/spec"
write_shared_case "$work_dir/ui"

# These all-selected fixtures deliberately put the failing case first in
# deterministic source order, followed by a passing case. If the validator is
# bypassed, libtest2 receives two Trial values with the same key while running
# with two workers. Its completion map then loses one handle and can print a
# recorded failure while still exiting zero. The expected path is the
# validator's exact pre-scheduling boundary, not that false-green outcome.
write_all_selected_shared_case() {
    directory="$1"
    cat > "$directory/a-failing.toml" <<EOF
[section]
id = "duplicate.concurrent"
name = "Concurrent duplicates"

[[case]]
name = "same"
source = "fn main() -> i32 { 0 }"
exit_code = 1
only_on = ["$host"]
EOF
    cat > "$directory/b-passing.toml" <<EOF
[section]
id = "duplicate.concurrent"
name = "Concurrent duplicates"

[[case]]
name = "same"
source = "fn main() -> i32 { 0 }"
exit_code = 0
only_on = ["$host"]
EOF
}

write_all_selected_shared_case "$work_dir/spec-all"
write_all_selected_shared_case "$work_dir/ui-all"

cat > "$work_dir/cli/a.toml" <<EOF
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
id = "cli.examples"
name = "CLI duplicates"

[[case]]
name = "demo"
only_on = ["$host"]
files = [{ path = "main.rue", source = "fn main() -> i32 { 0 }" }]
exit_code = 0
EOF
cat > "$work_dir/cli/b.toml" <<EOF
[section]
id = "cli.examples"
name = "CLI duplicates"

[[case]]
name = "demo"
only_on = ["$foreign"]
files = [{ path = "main.rue", source = "fn main() -> i32 { 0 }" }]
exit_code = 1
EOF
cat > "$work_dir/examples/demo.rue" <<'EOF'
fn main() -> i32 { 0 }
EOF

cat > "$work_dir/cli-all/a-failing.toml" <<EOF
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
id = "cli.concurrent"
name = "CLI concurrent duplicates"

[[case]]
name = "same"
only_on = ["$host"]
files = [{ path = "main.rue", source = "fn main() -> i32 { 0 }" }]
exit_code = 1
EOF
cat > "$work_dir/cli-all/b-passing.toml" <<EOF
[section]
id = "cli.concurrent"
name = "CLI concurrent duplicates"

[[case]]
name = "same"
only_on = ["$host"]
files = [{ path = "main.rue", source = "fn main() -> i32 { 0 }" }]
exit_code = 0
EOF

check_rejected() {
    label="$1"
    expected_name="$2"
    expected_a="$3"
    expected_b="$4"
    expected_a_case="$5"
    expected_b_case="$6"
    expected_extra="$7"
    expected_extra_case="$8"
    shift 8

    set +e
    output="$("$@" 2>&1)"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        echo "$label unexpectedly passed; duplicate reached the harness" >&2
        echo "$output" >&2
        exit 1
    fi
    expected_line="  - duplicate test name '$expected_name': $expected_a (case '$expected_a_case'); $expected_b (case '$expected_b_case')"
    if [ -n "$expected_extra" ]; then
        expected_line="$expected_line; $expected_extra (case '$expected_extra_case')"
    fi
    printf '%s\n' "$output" | grep -F "$expected_line" >/dev/null
    if printf '%s\n' "$output" | grep -F "running " >/dev/null; then
        echo "$label entered libtest2 before reporting the duplicate" >&2
        echo "$output" >&2
        exit 1
    fi
}

check_rejected \
    spec \
    'duplicate.section::same' \
    "$work_dir/spec/a.toml" \
    "$work_dir/spec/b.toml" \
    same \
    same \
    '' \
    '' \
    env RUE_BINARY="$RUE_BINARY" RUE_SPEC_CASES="$work_dir/spec" \
        RUE_PLATFORM_CASE_SELECTION=native "$RUE_SPEC_HARNESS" --test-threads=2 --quiet

check_rejected \
    ui \
    'duplicate.section::same' \
    "$work_dir/ui/a.toml" \
    "$work_dir/ui/b.toml" \
    same \
    same \
    '' \
    '' \
    env RUE_BINARY="$RUE_BINARY" RUE_UI_CASES="$work_dir/ui" \
        RUE_PLATFORM_CASE_SELECTION=native "$RUE_UI_HARNESS" --test-threads=2 --quiet

check_rejected \
    spec-all-selected \
    'duplicate.concurrent::same' \
    "$work_dir/spec-all/a-failing.toml" \
    "$work_dir/spec-all/b-passing.toml" \
    same \
    same \
    '' \
    '' \
    env RUE_BINARY="$RUE_BINARY" RUE_SPEC_CASES="$work_dir/spec-all" \
        RUE_PLATFORM_CASE_SELECTION=native "$RUE_SPEC_HARNESS" --test-threads=2 --quiet

check_rejected \
    ui-all-selected \
    'duplicate.concurrent::same' \
    "$work_dir/ui-all/a-failing.toml" \
    "$work_dir/ui-all/b-passing.toml" \
    same \
    same \
    '' \
    '' \
    env RUE_BINARY="$RUE_BINARY" RUE_UI_CASES="$work_dir/ui-all" \
        RUE_PLATFORM_CASE_SELECTION=native "$RUE_UI_HARNESS" --test-threads=2 --quiet

check_rejected \
    cli \
    'cli.examples::demo' \
    "$work_dir/cli/a.toml" \
    "$work_dir/cli/b.toml" \
    demo \
    demo \
    "$work_dir/examples/demo.rue" \
    demo.rue \
    env RUE_BINARY="$RUE_BINARY" RUE_CLI_CASES="$work_dir/cli" \
        RUE_EXAMPLES_DIR="$work_dir/examples" RUE_STD_DIR="$work_dir/std" \
        RUE_REPO_DIR="$work_dir" RUE_CLI_CASE_TIER=premerge \
        RUE_PLATFORM_CASE_SELECTION=native "$RUE_CLI_HARNESS" --test-threads=2 --quiet

check_rejected \
    cli-all-selected \
    'cli.concurrent::same' \
    "$work_dir/cli-all/a-failing.toml" \
    "$work_dir/cli-all/b-passing.toml" \
    same \
    same \
    '' \
    '' \
    env RUE_BINARY="$RUE_BINARY" RUE_CLI_CASES="$work_dir/cli-all" \
        RUE_EXAMPLES_DIR="$work_dir/examples" RUE_STD_DIR="$work_dir/std" \
        RUE_REPO_DIR="$work_dir" RUE_CLI_CASE_TIER=premerge \
        RUE_PLATFORM_CASE_SELECTION=native "$RUE_CLI_HARNESS" --test-threads=2 --quiet
