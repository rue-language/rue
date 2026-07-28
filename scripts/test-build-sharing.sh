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

test_lock_recovers_stale_holder() {
    local sb lock rc=0
    sb="$(mktemp -d)"; lock="$sb/full.lock"
    mkdir -m 700 "$lock"
    printf 'pid=99999999\ntoken=dead\nrepo=gone\n' >"$lock/owner"
    RUE_FULL_SUITE_LOCK_DIR="$lock" RUE_FULL_SUITE_LOCK_POLL_SECONDS=1 \
        "$SRC_ROOT/scripts/with-full-suite-lock" sh -c ": >'$sb/ran'" >/dev/null 2>&1 || rc=$?
    check "lock: a dead holder is recovered" \
        "$([ "$rc" -eq 0 ] && [[ -f "$sb/ran" ]] && [[ ! -e "$lock" ]] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_lock_recovers_uninitialized_orphan() {
    local sb lock rc=0
    sb="$(mktemp -d)"; lock="$sb/full.lock"
    mkdir -m 700 "$lock"
    RUE_FULL_SUITE_LOCK_DIR="$lock" RUE_FULL_SUITE_LOCK_INIT_GRACE_SECONDS=0 \
        "$SRC_ROOT/scripts/with-full-suite-lock" sh -c ": >'$sb/ran'" >/dev/null 2>&1 || rc=$?
    check "lock: an orphan without owner metadata is recovered" \
        "$([ "$rc" -eq 0 ] && [[ -f "$sb/ran" ]] && [[ ! -e "$lock" ]] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_lock_interrupt_releases() {
    local sb lock holder
    sb="$(mktemp -d)"; lock="$sb/full.lock"
    cat >"$sb/wait" <<'EOF'
#!/usr/bin/env bash
trap 'exit 0' TERM INT
: >"$READY"
while true; do sleep 1; done
EOF
    chmod +x "$sb/wait"
    READY="$sb/ready" RUE_FULL_SUITE_LOCK_DIR="$lock" \
        "$SRC_ROOT/scripts/with-full-suite-lock" "$sb/wait" >/dev/null 2>&1 &
    holder=$!
    for _ in {1..100}; do [[ -e "$sb/ready" ]] && break; sleep 0.02; done
    kill -TERM "$holder"
    wait "$holder" 2>/dev/null || true
    check "lock: interruption releases ownership" "$([[ ! -e "$lock" ]] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_lock_serializes_and_reports_once() {
    local sb lock first second
    sb="$(mktemp -d)"; lock="$sb/full.lock"
    cat >"$sb/hold" <<'EOF'
#!/usr/bin/env bash
: >"$READY"
while [[ ! -e "$RELEASE" ]]; do sleep 0.05; done
EOF
    chmod +x "$sb/hold"
    READY="$sb/ready" RELEASE="$sb/release" RUE_FULL_SUITE_LOCK_DIR="$lock" \
        "$SRC_ROOT/scripts/with-full-suite-lock" "$sb/hold" >"$sb/first.out" 2>&1 &
    first=$!
    for _ in {1..100}; do [[ -e "$sb/ready" ]] && break; sleep 0.02; done

    RUE_FULL_SUITE_LOCK_DIR="$lock" RUE_FULL_SUITE_LOCK_POLL_SECONDS=1 \
        RUE_FULL_SUITE_LOCK_NOTICE_SECONDS=60 \
        "$SRC_ROOT/scripts/with-full-suite-lock" sh -c ": >'$sb/second-ran'" >"$sb/second.out" 2>&1 &
    second=$!
    sleep 0.1
    check "lock: a second full suite cannot overlap" "$([[ ! -e "$sb/second-ran" ]] && echo 0 || echo 1)"
    : >"$sb/release"
    wait "$first" || true
    wait "$second" || true
    check "lock: waiter runs after release" "$([[ -e "$sb/second-ran" ]] && echo 0 || echo 1)"
    check "lock: waiting is observable without noisy polling" \
        "$([ "$(grep -c 'waiting...' "$sb/second.out" 2>/dev/null || true)" -eq 1 ] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_full_suite_orchestration() {
    local sb out rc=0
    sb="$(mktemp -d)"
    mkdir -p "$sb/scripts"
    cp "$SRC_ROOT/test.sh" "$sb/test.sh"
    cp "$SRC_ROOT/scripts/ci-heavy-suite" "$sb/scripts/ci-heavy-suite"
    cp "$SRC_ROOT/scripts/with-full-suite-lock" "$sb/scripts/with-full-suite-lock"
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
    chmod +x "$sb/test.sh" "$sb/scripts/ci-heavy-suite" \
        "$sb/scripts/cli-timeout-policy.py" "$sb/scripts/with-full-suite-lock"
    cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$BUCK_LOG"
if [[ "${1:-}" == "uquery" ]]; then
    # RUE-1116: test.sh subtracts the rue_cli_shard set from heavy-suite
    # discovery; this baseline fixture defines no CLI shards.
    if [[ "$*" == *rue_cli_shard* ]]; then
        :
    else
        printf '%s\n' \
            root//:cli-tests \
            root//:cli-tests-slow \
            root//:frontend-diff-test \
            root//:oracle-diff-generated-smoke \
            root//:reproducible-programs \
            root//:spec-tests \
            root//:ui-tests
    fi
elif [[ "${1:-}" == "test" ]]; then
    # Emit buck2's per-test result line so the RUE-924 corpus-omission audit
    # in test.sh sees each required harness actually ran. The heavy-labeled
    # corpus suites arrive one at a time through ci-heavy-suite; the
    # non-heavy tutorial-snippet-tests runs inside the broad "test //..." pass.
    if [[ "${2:-}" == "//..." ]]; then
        printf 'Pass: root//:tutorial-snippet-tests (0.0s)\n'
        printf 'Pass: root//:large-example-caldera-canary (0.0s)\n'
        printf 'Pass: root//:large-example-meridian-canary (0.0s)\n'
    elif [[ "${2:-}" == //:* ]]; then
        printf 'Pass: root%s (0.0s)\n' "${2:-}"
    else
        printf 'Pass: %s (0.0s)\n' "${2:-}"
    fi
fi
EOF
    chmod +x "$sb/buck2"
    # The required macOS job sets this variable for its outer test.sh. Keep
    # this fixture's baseline full-suite contract independent of that caller
    # environment; deferral behavior has its own focused tests.
    out="$(BUCK_LOG="$sb/calls" RUE_CI_DEFER_HEAVY_SUITES= \
        RUE_FULL_SUITE_LOCK_DIR="$sb/lock" "$sb/test.sh" 2>&1)" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
        printf '%s\n' "$out" >&2
    fi
    check "suite: unfiltered orchestration succeeds" "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
    check "suite: broad discovery excludes opaque heavy tests" \
        "$(grep -Fxq 'test //... toolchains//... --exclude rue_heavy_suite --always-exclude --ignore-tests-attribute --exclude rue_test_tier_stress' "$sb/calls" && echo 0 || echo 1)"
    check "suite: heavy targets are discovered from the live graph" \
        "$(grep -Fxq 'uquery attrfilter(labels, rue_heavy_suite, //...)' "$sb/calls" && echo 0 || echo 1)"
    check "suite: premerge CLI corpus receives the derived executor timeout" \
        "$(grep -Fxq 'test //:cli-tests -- --timeout 3600' "$sb/calls" && echo 0 || echo 1)"
    check "suite: slow CLI corpus receives the declarative executor timeout" \
        "$(grep -Fxq 'test //:cli-tests-slow -- --timeout 7200' "$sb/calls" && echo 0 || echo 1)"
    check "suite: every other heavy target uses the default executor timeout" \
        "$([ "$(grep -Ec '^test //:(spec-tests|ui-tests|oracle-diff-generated-smoke|reproducible-programs|frontend-diff-test)$' "$sb/calls")" -eq 5 ] && echo 0 || echo 1)"
    check "suite: no concurrent heavy label sweep occurs" \
        "$(! grep -Fq -- '--include rue_heavy_suite' "$sb/calls" && echo 0 || echo 1)"
    check "suite: host lock is released" "$([[ ! -e "$sb/lock" ]] && echo 0 || echo 1)"
    rm -rf "$sb"
}

test_cache_provisioning
test_buck_wrapper_prefers_local_cache_misses
test_buck_wrapper_auto_provisions
test_lock_recovers_stale_holder
test_lock_recovers_uninitialized_orphan
test_lock_interrupt_releases
test_lock_serializes_and_reports_once
test_full_suite_orchestration

echo "--------------------------------------------------"
if [[ "$FAILURES" -eq 0 ]]; then
    echo "build-sharing tests: all $TESTS checks passed"
    exit 0
fi
echo "build-sharing tests: $FAILURES of $TESTS checks FAILED"
exit 1
