#!/usr/bin/env bash
# Hermetic regression tests for cache provisioning and full-suite scheduling.
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

make_rue_root() {
    local root="$1"
    mkdir -p "$root/platforms"
    : >"$root/.buckconfig"
    : >"$root/platforms/remote_cache.bzl"
    printf '#!/bin/sh\nexit 0\n' >"$root/buck2"
    chmod +x "$root/buck2"
}

test_cache_provisioning() {
    local sb key config out rc mode
    sb="$(mktemp -d)"
    key='test-secret-that-must-not-be-printed'
    config="$sb/config/rue/buildbuddy.buckconfig"
    make_rue_root "$sb/primary"
    make_rue_root "$sb/worktree"

    mkdir -p "$(dirname "$config")"
    cat >"$config" <<EOF
[buck2_re_client]
http_headers = x-buildbuddy-api-key:$key
EOF
    chmod 600 "$config"
    rc=0
    HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        "$SRC_ROOT/scripts/provision-build-cache" status "$sb/primary" >/dev/null 2>&1 || rc=$?
    check "cache: pre-upload-gate configs require migration" \
        "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"

    rc=0
    out="$(HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" BUILDBUDDY_API_KEY="$key" \
        "$SRC_ROOT/scripts/provision-build-cache" install 2>&1)" || rc=$?
    check "cache: install succeeds" "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
    check "cache: install never prints the key" "$([[ "$out" != *"$key"* ]] && echo 0 || echo 1)"
    mode="$(stat -c '%a' "$config" 2>/dev/null || stat -f '%Lp' "$config")"
    check "cache: central config is private" "$([ $((8#$mode & 077)) -eq 0 ] && echo 0 || echo 1)"
    check "cache: Rust action uploads are enabled" \
        "$(grep -Eq '^default_allow_cache_upload = true$' "$config" && echo 0 || echo 1)"

    rc=0
    out="$(HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        "$SRC_ROOT/scripts/provision-build-cache" apply "$sb/primary" "$sb/worktree" 2>&1)" || rc=$?
    check "cache: one command provisions multiple worktrees" \
        "$([ "$rc" -eq 0 ] && [[ -L "$sb/primary/.buckconfig.local" ]] && [[ -L "$sb/worktree/.buckconfig.local" ]] && echo 0 || echo 1)"
    check "cache: worktrees share the central config" \
        "$([ "$(readlink "$sb/primary/.buckconfig.local")" = "$config" ] && [ "$(readlink "$sb/worktree/.buckconfig.local")" = "$config" ] && echo 0 || echo 1)"
    check "cache: provisioning never prints the key" "$([[ "$out" != *"$key"* ]] && echo 0 || echo 1)"

    make_rue_root "$sb/future-worktree"
    rc=0
    (cd "$sb/future-worktree" && HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        "$SRC_ROOT/scripts/provision-build-cache" auto) >/dev/null 2>&1 || rc=$?
    check "cache: a future worktree adopts the installed config on first use" \
        "$([ "$rc" -eq 0 ] && [[ -L "$sb/future-worktree/.buckconfig.local" ]] && echo 0 || echo 1)"

    rm -f "$sb/worktree/.buckconfig.local"
    printf 'local config that must survive\n' >"$sb/worktree/.buckconfig.local"
    rc=0
    HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        "$SRC_ROOT/scripts/provision-build-cache" apply "$sb/worktree" >/dev/null 2>&1 || rc=$?
    check "cache: existing per-worktree config is preserved" \
        "$([ "$rc" -ne 0 ] && grep -q 'must survive' "$sb/worktree/.buckconfig.local" && echo 0 || echo 1)"

    chmod 644 "$config"
    rc=0
    HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        "$SRC_ROOT/scripts/provision-build-cache" apply "$sb/primary" >/dev/null 2>&1 || rc=$?
    check "cache: insecure central permissions fail closed" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_buck_wrapper_prefers_local_cache_misses() {
    local sb
    sb="$(mktemp -d)"
    mkdir -p "$sb/fakebin"
    cp "$SRC_ROOT/buck2" "$sb/buck2"
    cp "$SRC_ROOT/buck2-bin" "$sb/buck2-bin"
    chmod +x "$sb/buck2"
    cat >"$sb/.buckconfig.local" <<'EOF'
[build]
execution_platforms = root//platforms:remote_cache
EOF
    cat >"$sb/fakebin/dotslash" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$DOTSLASH_ARGS"
EOF
    chmod +x "$sb/fakebin/dotslash"

    DOTSLASH_ARGS="$sb/build.args" PATH="$sb/fakebin:$PATH" "$sb/buck2" build //:probe
    check "buck wrapper: cache misses prefer local execution" \
        "$(grep -Fxq -- '--prefer-local' "$sb/build.args" && echo 0 || echo 1)"

    DOTSLASH_ARGS="$sb/remote.args" PATH="$sb/fakebin:$PATH" "$sb/buck2" build --prefer-remote //:probe
    check "buck wrapper: explicit execution preference is preserved" \
        "$(! grep -Fxq -- '--prefer-local' "$sb/remote.args" && grep -Fxq -- '--prefer-remote' "$sb/remote.args" && echo 0 || echo 1)"

    DOTSLASH_ARGS="$sb/run.args" PATH="$sb/fakebin:$PATH" "$sb/buck2" run //:probe -- program-arg
    local local_line separator_line
    local_line="$(grep -nFx -- '--prefer-local' "$sb/run.args" | cut -d: -f1)"
    separator_line="$(grep -nFx -- '--' "$sb/run.args" | cut -d: -f1)"
    check "buck wrapper: preference stays before executable arguments" \
        "$([ "$local_line" -lt "$separator_line" ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_buck_wrapper_auto_provisions() {
    local sb config
    sb="$(mktemp -d)"
    config="$sb/config/rue/buildbuddy.buckconfig"
    mkdir -p "$sb/scripts" "$sb/platforms" "$sb/fakebin" "$(dirname "$config")"
    cp "$SRC_ROOT/buck2" "$sb/buck2"
    cp "$SRC_ROOT/buck2-bin" "$sb/buck2-bin"
    cp "$SRC_ROOT/scripts/provision-build-cache" "$sb/scripts/provision-build-cache"
    : >"$sb/.buckconfig"
    : >"$sb/platforms/remote_cache.bzl"
    chmod +x "$sb/buck2" "$sb/scripts/provision-build-cache"
    cat >"$config" <<'EOF'
[buck2_re_client]
http_headers = x-buildbuddy-api-key:test-secret
[buck2]
default_allow_cache_upload = true
[build]
execution_platforms = root//platforms:remote_cache
EOF
    chmod 600 "$config"
    cat >"$sb/fakebin/dotslash" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$@" >"$DOTSLASH_ARGS"
EOF
    chmod +x "$sb/fakebin/dotslash"

    (cd "$sb" && HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        DOTSLASH_ARGS="$sb/args" PATH="$sb/fakebin:$PATH" ./buck2 build //:probe)
    check "buck wrapper: direct use provisions a future worktree" \
        "$([[ -L "$sb/.buckconfig.local" ]] && [ "$(readlink "$sb/.buckconfig.local")" = "$config" ] && echo 0 || echo 1)"
    check "buck wrapper: newly adopted cache prefers local misses" \
        "$(grep -Fxq -- '--prefer-local' "$sb/args" && echo 0 || echo 1)"

    rm "$sb/.buckconfig.local"
    ln -s "$sb/missing-user-config" "$sb/.buckconfig.local"
    (cd "$sb" && HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" \
        DOTSLASH_ARGS="$sb/broken.args" PATH="$sb/fakebin:$PATH" ./buck2 build //:probe)
    check "buck wrapper: a broken user symlink never blocks local builds" \
        "$([[ -L "$sb/.buckconfig.local" ]] && [ "$(readlink "$sb/.buckconfig.local")" = "$sb/missing-user-config" ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_full_suite_orchestration() {
    local sb out rc=0
    sb="$(mktemp -d)"
    mkdir -p "$sb/scripts"
    cp "$SRC_ROOT/test.sh" "$sb/test.sh"
    cp "$SRC_ROOT/scripts/ci-heavy-suite" "$sb/scripts/ci-heavy-suite"
    cat >"$sb/scripts/cli-timeout-policy.py" <<'EOF'
#!/usr/bin/env bash
target=""
while [[ "$#" -gt 0 ]]; do
    if [[ "$1" == "--target" ]]; then
        target="$2"
        shift 2
    else
        shift
    fi
done
case "$target" in
    //:cli-tests) echo 3600 ;;
    //:cli-tests-slow) echo 7200 ;;
    *) exit 2 ;;
esac
EOF
    chmod +x "$sb/test.sh" "$sb/scripts/ci-heavy-suite" "$sb/scripts/cli-timeout-policy.py"
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

test_cache_provisioning
test_buck_wrapper_prefers_local_cache_misses
test_buck_wrapper_auto_provisions
test_full_suite_orchestration

echo "--------------------------------------------------"
if [[ "$FAILURES" -eq 0 ]]; then
    echo "build-sharing tests: all $TESTS checks passed"
    exit 0
fi
echo "build-sharing tests: $FAILURES of $TESTS checks FAILED"
exit 1
