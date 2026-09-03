#!/usr/bin/env bash
# Hermetic regression tests for cache provisioning, the ./buck2 wrapper's
# remote-cache handling, and full-suite scheduling.
set -uo pipefail

if [[ -n "${RUE_BUILD_SHARING_ROOT:-}" ]]; then
    SRC_ROOT="$RUE_BUILD_SHARING_ROOT"
else
    SRC_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fi

FAILURES=0
TESTS=0
fail() { printf 'FAIL: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }
pass() { printf 'ok: %s\n' "$1"; }
check() {
    TESTS=$((TESTS + 1))
    if [[ "$2" -eq 0 ]]; then pass "$1"; else fail "$1"; fi
}

# The real ./buck2 and its DotSlash manifest, a fake dotslash that records the
# argv it would have run, and a fake `df -Pk` reporting ample free space so
# the wrapper's free-space floor (pinned by test-cleanup-scripts.sh) never
# decides these tests.
make_wrapper_sandbox() {
    local sb="$1"
    mkdir -p "$sb/fakebin" "$sb/config"
    cp "$SRC_ROOT/buck2" "$sb/buck2"
    cp "$SRC_ROOT/buck2-bin" "$sb/buck2-bin"
    chmod +x "$sb/buck2"
    # The first argument is the DotSlash manifest; the log holds Buck's argv.
    cat >"$sb/fakebin/dotslash" <<'EOF'
#!/usr/bin/env bash
shift
printf '%s\n' "$@" >"$DOTSLASH_ARGS"
EOF
    cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf '/dev/fake 100000000 1000 50000000 1%% /\n'
EOF
    chmod +x "$sb/fakebin/dotslash" "$sb/fakebin/df"
}

# run_wrapper <sandbox> <argv-log> [buck2 args...]. HOME and XDG_CONFIG_HOME
# point into the sandbox and the override variables are cleared, so nothing
# installed on the host reaches the argv under test; WRAPPER_ENV adds
# assignments for one call.
run_wrapper() {
    local sb="$1" log="$2"
    shift 2
    ( cd "$sb" && unset RUE_BUILDBUDDY_CONFIG RUE_NO_REMOTE_CACHE &&
      env HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" DOTSLASH_ARGS="$sb/$log" \
          PATH="$sb/fakebin:$PATH" ${WRAPPER_ENV:-} ./buck2 "$@" ) >"$sb/$log.out" 2>&1
}

write_central_config() {
    mkdir -p "$(dirname "$1")"
    cat >"$1" <<'EOF'
[buck2_re_client]
http_headers = x-buildbuddy-api-key:test-secret
[buck2]
default_allow_cache_upload = true
[build]
execution_platforms = root//platforms:remote_cache
EOF
    chmod 600 "$1"
}

# 1-based line number of the first argv entry equal to $2 in log $1, or empty.
argv_line() {
    awk -v want="$2" '$0 == want { print NR; exit }' "$1"
}

test_cache_install() {
    local sb key config out rc mode
    sb="$(mktemp -d)"
    key='test-secret-that-must-not-be-printed'
    config="$sb/config/rue/buildbuddy.buckconfig"
    mkdir -p "$sb/checkout"

    rc=0
    out="$(cd "$sb/checkout" && HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" BUILDBUDDY_API_KEY="$key" \
        "$SRC_ROOT/scripts/provision-build-cache" install 2>&1)" || rc=$?
    check "cache: install succeeds" "$([ "$rc" -eq 0 ] && [[ -f "$config" ]] && echo 0 || echo 1)"
    check "cache: install never prints the key" "$([[ "$out" != *"$key"* ]] && echo 0 || echo 1)"
    mode="$(stat -c '%a' "$config" 2>/dev/null || stat -f '%Lp' "$config")"
    check "cache: central config is private" "$([ $((8#$mode & 077)) -eq 0 ] && echo 0 || echo 1)"
    check "cache: the credential is stored as the BuildBuddy header" \
        "$(grep -q "^http_headers = x-buildbuddy-api-key:$key\$" "$config" && echo 0 || echo 1)"
    check "cache: Rust action uploads are enabled" \
        "$(grep -Eq '^default_allow_cache_upload = true$' "$config" && echo 0 || echo 1)"
    check "cache: the remote-cache execution platform is selected" \
        "$(grep -Eq '^execution_platforms = root//platforms:remote_cache$' "$config" && echo 0 || echo 1)"
    check "cache: install writes nothing into the checkout" \
        "$([ ! -e "$sb/checkout/.buckconfig.local" ] && echo 0 || echo 1)"

    rc=0
    HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" BUILDBUDDY_API_KEY="$key-rotated" \
        "$SRC_ROOT/scripts/provision-build-cache" install >/dev/null 2>&1 || rc=$?
    check "cache: re-running install replaces the config in place" \
        "$([ "$rc" -eq 0 ] && grep -q "x-buildbuddy-api-key:$key-rotated\$" "$config" && ! grep -q "x-buildbuddy-api-key:$key\$" "$config" && echo 0 || echo 1)"
    mode="$(stat -c '%a' "$config" 2>/dev/null || stat -f '%Lp' "$config")"
    check "cache: the replacement is private and leaves no temporary file" \
        "$([ $((8#$mode & 077)) -eq 0 ] && [ "$(ls "$sb/config/rue" | wc -l | tr -d ' ')" -eq 1 ] && echo 0 || echo 1)"

    rc=0
    HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" BUILDBUDDY_API_KEY= \
        "$SRC_ROOT/scripts/provision-build-cache" install </dev/null >/dev/null 2>&1 || rc=$?
    check "cache: an empty key without a terminal is refused" \
        "$([ "$rc" -ne 0 ] && grep -q "x-buildbuddy-api-key:$key-rotated\$" "$config" && echo 0 || echo 1)"

    rc=0
    HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        "$SRC_ROOT/scripts/provision-build-cache" apply >/dev/null 2>&1 || rc=$?
    check "cache: install is the only subcommand" "$([ "$rc" -eq 2 ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_buck_wrapper_prefers_local_cache_misses() {
    local sb local_line separator_line
    sb="$(mktemp -d)"
    make_wrapper_sandbox "$sb"
    # A hand-written per-checkout config, which buck2 reads on its own; the
    # wrapper still recognizes it as a cache configuration.
    cat >"$sb/.buckconfig.local" <<'EOF'
[build]
execution_platforms = root//platforms:remote_cache
EOF

    run_wrapper "$sb" build.args build //:probe
    check "buck wrapper: cache misses prefer local execution" \
        "$(grep -Fxq -- '--prefer-local' "$sb/build.args" && echo 0 || echo 1)"
    check "buck wrapper: a per-checkout config is left as the user wrote it" \
        "$([ ! -L "$sb/.buckconfig.local" ] && grep -q 'remote_cache' "$sb/.buckconfig.local" && echo 0 || echo 1)"

    run_wrapper "$sb" remote.args build --prefer-remote //:probe
    check "buck wrapper: explicit execution preference is preserved" \
        "$(! grep -Fxq -- '--prefer-local' "$sb/remote.args" && grep -Fxq -- '--prefer-remote' "$sb/remote.args" && echo 0 || echo 1)"

    run_wrapper "$sb" run.args run //:probe -- program-arg
    local_line="$(argv_line "$sb/run.args" --prefer-local)"
    separator_line="$(argv_line "$sb/run.args" --)"
    check "buck wrapper: preference stays before executable arguments" \
        "$([ -n "$local_line" ] && [ "$local_line" -lt "$separator_line" ] && echo 0 || echo 1)"

    run_wrapper "$sb" clean.args clean
    check "buck wrapper: non-execution commands receive no execution preference" \
        "$(! grep -Fxq -- '--prefer-local' "$sb/clean.args" && echo 0 || echo 1)"
    rm -rf "$sb"
}

# The installed config reaches buck2 through a .buckconfig.local symlink, not
# a per-command --config-file: [buck2] digest_algorithms and [buck2_re_client]
# are daemon-startup settings that only a project config applies.
test_buck_wrapper_links_installed_config() {
    local sb config alt
    sb="$(mktemp -d)"
    make_wrapper_sandbox "$sb"
    config="$sb/config/rue/buildbuddy.buckconfig"
    write_central_config "$config"

    run_wrapper "$sb" audit.args audit config build.execution_platforms
    check "buck wrapper: non-execution commands never provision" \
        "$([ ! -e "$sb/.buckconfig.local" ] && [ ! -L "$sb/.buckconfig.local" ] && echo 0 || echo 1)"

    run_wrapper "$sb" build.args build //:probe
    check "buck wrapper: the first execution command links .buckconfig.local to the installed config" \
        "$([[ -L "$sb/.buckconfig.local" ]] && [ "$(readlink "$sb/.buckconfig.local")" = "$config" ] && echo 0 || echo 1)"
    check "buck wrapper: that first build already prefers local execution for misses" \
        "$(grep -Fxq -- '--prefer-local' "$sb/build.args" && echo 0 || echo 1)"
    check "buck wrapper: the config is never passed on the command line" \
        "$(! grep -Fxq -- '--config-file' "$sb/build.args" && echo 0 || echo 1)"

    run_wrapper "$sb" again.args test //:probe
    check "buck wrapper: an existing link is left as is" \
        "$([[ -L "$sb/.buckconfig.local" ]] && [ "$(readlink "$sb/.buckconfig.local")" = "$config" ] && grep -Fxq -- '--prefer-local' "$sb/again.args" && echo 0 || echo 1)"

    rm "$sb/.buckconfig.local"
    printf 'local config that must survive\n' >"$sb/.buckconfig.local"
    run_wrapper "$sb" keep.args build //:probe
    check "buck wrapper: an existing per-checkout config is never replaced" \
        "$([ ! -L "$sb/.buckconfig.local" ] && grep -q 'must survive' "$sb/.buckconfig.local" && [ "$(head -n 1 "$sb/keep.args")" = build ] && echo 0 || echo 1)"

    rm "$sb/.buckconfig.local"
    ln -s "$sb/missing-user-config" "$sb/.buckconfig.local"
    run_wrapper "$sb" broken.args build //:probe
    check "buck wrapper: a broken user symlink is left alone and never blocks local builds" \
        "$([[ -L "$sb/.buckconfig.local" ]] && [ "$(readlink "$sb/.buckconfig.local")" = "$sb/missing-user-config" ] && [ "$(head -n 1 "$sb/broken.args")" = build ] && echo 0 || echo 1)"
    rm "$sb/.buckconfig.local"

    WRAPPER_ENV="RUE_NO_REMOTE_CACHE=1" run_wrapper "$sb" optout.args build //:probe
    check "buck wrapper: RUE_NO_REMOTE_CACHE=1 links nothing and adds no preference" \
        "$([ ! -e "$sb/.buckconfig.local" ] && [ ! -L "$sb/.buckconfig.local" ] && [ "$(tr '\n' ' ' <"$sb/optout.args")" = 'build //:probe ' ] && echo 0 || echo 1)"

    chmod 644 "$config"
    run_wrapper "$sb" insecure.args build //:probe
    check "buck wrapper: a config readable by other accounts is not linked, with a warning" \
        "$([ ! -L "$sb/.buckconfig.local" ] && grep -q 'readable by other accounts' "$sb/insecure.args.out" && echo 0 || echo 1)"
    check "buck wrapper: refusing the config never blocks the build" \
        "$([ "$(tr '\n' ' ' <"$sb/insecure.args")" = 'build //:probe ' ] && echo 0 || echo 1)"
    chmod 600 "$config"

    alt="$sb/alt.buckconfig"
    write_central_config "$alt"
    WRAPPER_ENV="RUE_BUILDBUDDY_CONFIG=$alt" run_wrapper "$sb" alt.args build //:probe
    check "buck wrapper: RUE_BUILDBUDDY_CONFIG names the config to link" \
        "$([[ -L "$sb/.buckconfig.local" ]] && [ "$(readlink "$sb/.buckconfig.local")" = "$alt" ] && echo 0 || echo 1)"
    rm "$sb/.buckconfig.local"

    rm -f "$config"
    run_wrapper "$sb" absent.args build //:probe
    check "buck wrapper: without an installed config nothing is linked and the argv is unchanged" \
        "$([ ! -e "$sb/.buckconfig.local" ] && [ ! -L "$sb/.buckconfig.local" ] && [ "$(tr '\n' ' ' <"$sb/absent.args")" = 'build //:probe ' ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_full_suite_orchestration() {
    local sb out rc=0
    sb="$(mktemp -d)"
    mkdir -p "$sb/scripts"
    cp "$SRC_ROOT/test.sh" "$sb/test.sh"
    cp "$SRC_ROOT/scripts/ci-heavy-suite" "$sb/scripts/ci-heavy-suite"
    chmod +x "$sb/test.sh" "$sb/scripts/ci-heavy-suite"
    cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$BUCK_LOG"
if [[ "${1:-}" == "bxl" ]]; then
    # RUE-1163: heavy-suite membership comes from the BXL selector, not from a
    # label uquery filtered in bash.
    case "${2:-}" in
        *:validate) printf 'Rue test tiers valid\n' ;;
    esac
elif [[ "${1:-}" == "test" ]]; then
    printf 'Pass: %s (0.0s)\n' "${2:-}"
fi
EOF
    chmod +x "$sb/buck2"
    # The required macOS job sets this variable for its outer test.sh. Keep
    # this fixture's baseline full-suite contract independent of that caller
    # environment; deferral behavior has its own focused tests.
    out="$(BUCK_LOG="$sb/calls" "$sb/test.sh" 2>&1)" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        printf '%s\n' "$out" >&2
    fi
    check "suite: unfiltered orchestration succeeds" "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
    # RUE-1163: the full suite is one buck2 invocation. Selection is label
    # filters buck2 evaluates; scheduling is buck2's, because each corpus action
    # declares it needs the whole machine (corpus.bzl). The bash loop that ran
    # each corpus alone, and the host-wide lock that serialized full suites
    # across worktrees, are both gone.
    check "suite: the run is a single buck2 test invocation" \
        "$([ "$(grep -c '^test ' "$sb/calls")" -eq 1 ] && echo 0 || echo 1)"
    check "suite: discovery stays broad" \
        "$(grep -Fq 'test //... toolchains//...' "$sb/calls" && echo 0 || echo 1)"
    check "suite: heavy corpora are included rather than excluded" \
        "$(! grep -Fq -- '--exclude rue_heavy_suite' "$sb/calls" && echo 0 || echo 1)"
    check "suite: the CLI shards do not re-run the corpus" \
        "$(grep -Fq -- '--exclude rue_cli_shard' "$sb/calls" && echo 0 || echo 1)"
    check "suite: no corpus receives an executor timeout" \
        "$([ "$(grep -Ec -- '^test .* --timeout' "$sb/calls")" -eq 0 ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_cache_install
test_buck_wrapper_prefers_local_cache_misses
test_buck_wrapper_links_installed_config
test_full_suite_orchestration

echo "--------------------------------------------------"
if [[ "$FAILURES" -eq 0 ]]; then
    echo "build-sharing tests: all $TESTS checks passed"
    exit 0
fi
echo "build-sharing tests: $FAILURES of $TESTS checks FAILED"
exit 1
