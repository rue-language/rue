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
    # Retry tests provide numbered output/status fixtures and retain each
    # invocation's argv separately.
    cat >"$sb/fakebin/dotslash" <<'EOF'
#!/usr/bin/env bash
shift
if [[ -n "${DOTSLASH_ATTEMPT_PREFIX:-}" ]]; then
    count=0
    [[ -f "${DOTSLASH_ATTEMPT_PREFIX}.count" ]] && count="$(cat "${DOTSLASH_ATTEMPT_PREFIX}.count")"
    count=$((count + 1))
    printf '%s\n' "$count" >"${DOTSLASH_ATTEMPT_PREFIX}.count"
    printf '%s\n' "$@" >"$DOTSLASH_ARGS.$count"
    [[ ! -f "${DOTSLASH_ATTEMPT_PREFIX}.output.$count" ]] || cat "${DOTSLASH_ATTEMPT_PREFIX}.output.$count"
    [[ ! -f "${DOTSLASH_ATTEMPT_PREFIX}.stderr.$count" ]] || cat "${DOTSLASH_ATTEMPT_PREFIX}.stderr.$count" >&2
    status=0
    [[ ! -f "${DOTSLASH_ATTEMPT_PREFIX}.status.$count" ]] || status="$(cat "${DOTSLASH_ATTEMPT_PREFIX}.status.$count")"
    exit "$status"
fi
printf '%s\n' "$@" >"$DOTSLASH_ARGS"
EOF
    cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf '/dev/fake 100000000 1000 50000000 1%% /\n'
EOF
    chmod +x "$sb/fakebin/dotslash" "$sb/fakebin/df"
}

write_retry_attempt() {
    local sb="$1" attempt="$2" status="$3" output="$4"
    printf '%s\n' "$status" >"$sb/attempt.status.$attempt"
    printf '%s\n' "$output" >"$sb/attempt.output.$attempt"
}

write_retry_stderr_attempt() {
    local sb="$1" attempt="$2" output="$3"
    printf '%s\n' "$output" >"$sb/attempt.stderr.$attempt"
}

write_retry_failure() {
    local sb="$1" attempt="$2" status="$3" output="$4"
    write_retry_attempt "$sb" "$attempt" "$status" ""
    write_retry_stderr_attempt "$sb" "$attempt" "$output"
}

grpc_failure() {
    cat <<'EOF'
Internal error (stage: materialize_inputs_failed): Error materializing artifact at path `buck-out/v2/art/toolchains/01d1977e3655e393/zig/__dist-linux-x86_64__/dist-linux-x86_64/zig`
Error: (Failed to make BatchReadBlobs request: code: 'Unknown error', message: "protocol error: missing grpc-status trailer, stream was terminated without a final status (possible truncation by a proxy or load balancer)")
EOF
}

eof_failure() {
    cat <<'EOF'
Internal error (stage: materialize_inputs_failed): Error materializing artifact at path `buck-out/v2/art/toolchains/01d1977e3655e393/zig/__dist-linux-x86_64__/dist-linux-x86_64/zig`
Error: (Failed to make BatchReadBlobs request: Unexpected EOF decoding stream)
EOF
}

# RUE-1949's classifier matched only the Zig tree. The same transport failure
# lands on the rustc and rust-std distributions too, and those merge-group
# ejections were never retried (RUE-2003).
rust_tree_failure() {
    cat <<'EOF'
Internal error (stage: materialize_inputs_failed): Error materializing artifact at path `buck-out/v2/art/toolchains/01d1977e3655e393/rust/__rustc-linux-x86_64__/rustc-linux-x86_64/rustc/bin/rustc`
Error: (Failed to make BatchReadBlobs request: code: 'Internal error', message: "Unexpected EOF decoding stream.")
EOF
}

disjoint_grpc_failure() {
    cat <<'EOF'
Internal error (stage: materialize_inputs_failed): Error materializing artifact at path `buck-out/v2/art/toolchains/01d1977e3655e393/zig/__dist-linux-x86_64__/dist-linux-x86_64/zig`
unrelated action output separates the two diagnostics
Error: (Failed to make BatchReadBlobs request: code: 'Unknown error', message: "protocol error: missing grpc-status trailer, stream was terminated without a final status (possible truncation by a proxy or load balancer)")
EOF
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

run_wrapper_split() {
    local sb="$1" log="$2"
    shift 2
    ( cd "$sb" && unset RUE_BUILDBUDDY_CONFIG RUE_NO_REMOTE_CACHE &&
      env HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" DOTSLASH_ARGS="$sb/$log" \
          PATH="$sb/fakebin:$PATH" ${WRAPPER_ENV:-} ./buck2 "$@" ) >"$sb/$log.stdout" 2>"$sb/$log.stderr"
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
    check "cache: one BatchReadBlobs response is bounded" \
        "$(grep -Eq '^max_total_batch_size = 1000000$' "$config" &&
           grep -Eq '^max_decoding_message_size = 16000000$' "$config" && echo 0 || echo 1)"
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

test_buck_wrapper_retries_only_truncated_cas_materialization() {
    local sb out rc fixture variant missing preference_ok status_ok expected_output
    sb="$(mktemp -d)"
    make_wrapper_sandbox "$sb"
    cat >"$sb/.buckconfig.local" <<'EOF'
[build]
execution_platforms = root//platforms:remote_cache
EOF

    # Both transport endings observed for this incident classify, and a clean
    # replay returns success while retaining both attempts' output.
    for variant in grpc eof rust; do
        rm -f "$sb"/attempt.* "$sb"/retry.args.*
        case "$variant" in
            grpc) fixture="$(grpc_failure)" ;;
            eof) fixture="$(eof_failure)" ;;
            rust) fixture="$(rust_tree_failure)" ;;
        esac
        write_retry_attempt "$sb" 1 41 "first attempt stdout ($variant)"
        write_retry_stderr_attempt "$sb" 1 "$fixture"
        write_retry_attempt "$sb" 2 0 "second attempt succeeded ($variant)"
        rc=0
        if [[ "$variant" == grpc ]]; then
            WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build --prefer-remote //:probe --config cli.test=value || rc=$?
        else
            WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build //:probe --config cli.test=value || rc=$?
        fi
        out="$(cat "$sb/retry.args.out")"
        check "buck wrapper: $variant truncation retries and succeeds" \
            "$([ "$rc" -eq 0 ] && [ "$(cat "$sb/attempt.count")" -eq 2 ] && echo 0 || echo 1)"
        check "buck wrapper: $variant retry keeps both attempts visible" \
            "$([[ "$out" == *"materialize_inputs_failed"* ]] && [[ "$out" == *"second attempt succeeded ($variant)"* ]] && [[ "$out" == *"retrying Buck once"* ]] && echo 0 || echo 1)"
        if [[ "$variant" == grpc ]]; then
            preference_ok="$(grep -Fxq -- '--prefer-remote' "$sb/retry.args.1" && ! grep -Fxq -- '--prefer-local' "$sb/retry.args.1" && echo 0 || echo 1)"
        else
            preference_ok="$(grep -Fxq -- '--prefer-local' "$sb/retry.args.1" && echo 0 || echo 1)"
        fi
        check "buck wrapper: $variant retry replays exact post-wrapper argv" \
            "$(cmp -s "$sb/retry.args.1" "$sb/retry.args.2" && [ "$preference_ok" -eq 0 ] && grep -Fxq -- '--config' "$sb/retry.args.1" && grep -Fxq -- 'cli.test=value' "$sb/retry.args.1" && echo 0 || echo 1)"
    done

    # Capturing eligible commands must not merge streams: rue-bin parses
    # --show-output on stdout while forwarding Buck progress from stderr.
    rm -f "$sb"/attempt.* "$sb"/split.args.*
    write_retry_attempt "$sb" 1 0 "crates/rue:rue buck-out/v2/gen/root/rue"
    write_retry_stderr_attempt "$sb" 1 "BUILD SUCCEEDED"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper_split "$sb" split.args build //crates/rue:rue --show-output || rc=$?
    check "buck wrapper: eligible capture preserves stdout and stderr channels" \
        "$([ "$rc" -eq 0 ] && grep -Fxq 'crates/rue:rue buck-out/v2/gen/root/rue' "$sb/split.args.stdout" && grep -Fxq 'BUILD SUCCEEDED' "$sb/split.args.stderr" && ! grep -Fq 'BUILD SUCCEEDED' "$sb/split.args.stdout" && ! grep -Fq 'crates/rue:rue' "$sb/split.args.stderr" && echo 0 || echo 1)"

    # Local commands retain direct exec behavior even with a cache config. A
    # program cannot trigger the CI-only retry by printing the signature.
    rm -f "$sb"/attempt.* "$sb"/local.args.*
    write_retry_failure "$sb" 1 26 "$(grpc_failure)"
    rc=0
    WRAPPER_ENV="DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" local.args run //:probe || rc=$?
    check "buck wrapper: configured local commands are never captured or retried" \
        "$([ "$rc" -eq 26 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && echo 0 || echo 1)"

    # Classification is one adjacent Buck stderr diagnostic pair, not an
    # order-independent bag of words from program output or unrelated lines.
    for variant in stdout disjoint; do
        rm -f "$sb"/attempt.* "$sb"/retry.args.*
        if [[ "$variant" == stdout ]]; then
            write_retry_attempt "$sb" 1 27 "$(grpc_failure)"
        else
            write_retry_failure "$sb" 1 28 "$(disjoint_grpc_failure)"
        fi
        rc=0
        WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args run //:probe || rc=$?
        check "buck wrapper: $variant signature is not retried" \
            "$([ "$(cat "$sb/attempt.count")" -eq 1 ] && { [ "$rc" -eq 27 ] || [ "$rc" -eq 28 ]; } && echo 0 || echo 1)"
    done

    # Program arguments after `--` are not Buck flags. They cannot disable the
    # retry or suppress the wrapper's default --prefer-local insertion.
    rm -f "$sb"/attempt.* "$sb"/separator.args.*
    write_retry_failure "$sb" 1 29 "$(grpc_failure)"
    write_retry_attempt "$sb" 2 0 "separator retry succeeded"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" separator.args run //:probe -- --no-remote-cache --local-only --prefer-remote || rc=$?
    check "buck wrapper: program arguments after separator do not change retry policy" \
        "$([ "$rc" -eq 0 ] && [ "$(cat "$sb/attempt.count")" -eq 2 ] && grep -Fxq -- '--prefer-local' "$sb/separator.args.1" && cmp -s "$sb/separator.args.1" "$sb/separator.args.2" && echo 0 || echo 1)"

    # A matching second failure is returned directly, proving both failure
    # propagation and the hard ceiling of one replay.
    rm -f "$sb"/attempt.* "$sb"/retry.args.*
    write_retry_failure "$sb" 1 42 "$(eof_failure)"
    write_retry_failure "$sb" 2 73 "$(grpc_failure)"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args test //:probe || rc=$?
    check "buck wrapper: retry failure preserves the original status" "$([ "$rc" -eq 42 ] && echo 0 || echo 1)"
    check "buck wrapper: a matching retry is never retried again" \
        "$([ "$(cat "$sb/attempt.count")" -eq 2 ] && [[ "$(cat "$sb/retry.args.out")" == *"Failed to make BatchReadBlobs request"* ]] && echo 0 || echo 1)"

    # Success and ordinary compiler/test failures retain their original one-run
    # behavior even while the remote cache is configured.
    for variant in success compiler; do
        rm -f "$sb"/attempt.* "$sb"/retry.args.*
        if [[ "$variant" == success ]]; then
            write_retry_attempt "$sb" 1 0 "BUILD SUCCEEDED"
            expected_output="BUILD SUCCEEDED"
        else
            write_retry_attempt "$sb" 1 19 "Action failed: rustc\nerror[E0308]: mismatched types\ntest result: FAILED"
            expected_output="error[E0308]"
        fi
        rc=0
        WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build //:probe || rc=$?
        if [[ "$variant" == success ]]; then status_ok="$([ "$rc" -eq 0 ] && echo 0 || echo 1)"; else status_ok="$([ "$rc" -eq 19 ] && echo 0 || echo 1)"; fi
        check "buck wrapper: $variant output is not retried" \
            "$([ "$status_ok" -eq 0 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && grep -Fq "$expected_output" "$sb/retry.args.out" && echo 0 || echo 1)"
    done

    # Removing any one invariant from the conjunction fails closed.
    for missing in stage path batch transport; do
        rm -f "$sb"/attempt.* "$sb"/retry.args.*
        fixture="$(grpc_failure)"
        case "$missing" in
            stage) fixture="${fixture/materialize_inputs_failed/materialize_failed}" ;;
            path) fixture="${fixture/Error materializing artifact at path/Error preparing artifact}" ;;
            batch) fixture="${fixture/BatchReadBlobs/ReadBlob}" ;;
            transport) fixture="${fixture/missing grpc-status trailer, stream was terminated without a final status (possible truncation by a proxy or load balancer)/connection reset by peer}" ;;
        esac
        write_retry_failure "$sb" 1 31 "$fixture"
        rc=0
        WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build //:probe || rc=$?
        check "buck wrapper: signature without $missing is not retried" \
            "$([ "$rc" -eq 31 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && echo 0 || echo 1)"
    done

    # The same diagnostic is not eligible without an active cache config or
    # under the per-command cache opt-out.
    rm "$sb/.buckconfig.local"
    rm -f "$sb"/attempt.* "$sb"/retry.args.*
    write_retry_failure "$sb" 1 51 "$(grpc_failure)"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build //:probe || rc=$?
    check "buck wrapper: matching output without cache configuration is not retried" \
        "$([ "$rc" -eq 51 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && echo 0 || echo 1)"

    cat >"$sb/.buckconfig.local" <<'EOF'
[build]
execution_platforms = root//platforms:remote_cache
EOF
    rm -f "$sb"/attempt.* "$sb"/retry.args.*
    write_retry_failure "$sb" 1 52 "$(eof_failure)"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true RUE_NO_REMOTE_CACHE=1 DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build //:probe || rc=$?
    check "buck wrapper: cache opt-out is not retried" \
        "$([ "$rc" -eq 52 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && echo 0 || echo 1)"

    rm -f "$sb"/attempt.* "$sb"/retry.args.*
    write_retry_failure "$sb" 1 53 "$(grpc_failure)"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build --no-remote-cache //:probe || rc=$?
    check "buck wrapper: Buck's no-remote-cache flag is not retried" \
        "$([ "$rc" -eq 53 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && echo 0 || echo 1)"

    rm -f "$sb"/attempt.* "$sb"/retry.args.*
    write_retry_failure "$sb" 1 54 "$(eof_failure)"
    rc=0
    WRAPPER_ENV="GITHUB_ACTIONS=true DOTSLASH_ATTEMPT_PREFIX=$sb/attempt" run_wrapper "$sb" retry.args build --local-only //:probe || rc=$?
    check "buck wrapper: local-only execution is not retried" \
        "$([ "$rc" -eq 54 ] && [ "$(cat "$sb/attempt.count")" -eq 1 ] && echo 0 || echo 1)"

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
test_buck_wrapper_retries_only_truncated_cas_materialization
test_full_suite_orchestration

echo "--------------------------------------------------"
if [[ "$FAILURES" -eq 0 ]]; then
    echo "build-sharing tests: all $TESTS checks passed"
    exit 0
fi
echo "build-sharing tests: $FAILURES of $TESTS checks FAILED"
exit 1
