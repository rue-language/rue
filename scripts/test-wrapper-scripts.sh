#!/usr/bin/env bash
# test-wrapper-scripts.sh — regressions for the developer wrapper scripts.
#
# Two failure modes found during the autonomous bug hunt:
#
#   * RUE-550: Buck stderr was discarded inside `set -euo pipefail` assignments
#     (`X="$(./buck2 ... 2>/dev/null | awk ...)"`). A build/toolchain failure
#     therefore exited the resolver scripts (rue-bin, fmt.sh) AT THE ASSIGNMENT,
#     before any diagnostic fallback could run — zero bytes on stdout AND
#     stderr. Every higher-level wrapper obtains the compiler through
#     `$(scripts/rue-bin)`, so the failure was invisible everywhere.
#
#   * RUE-549: `scripts/rue run|exec` cd'd to the repo root before forwarding
#     relative <source.rue> / `-o` paths to the compiler, so they resolved from
#     the wrong directory even though the script advertises "run from anywhere".
#
#   * RUE-537: filtered CLI wrappers handed the harness a relative examples
#     directory. The harness later runs each compiler case from a temporary
#     directory, where that repository-relative path no longer resolves.
#
#   * RUE-590: the Valgrind driver compiled repository examples without the
#     bundled standard-library path, so a top-level example using @import("std")
#     failed before Valgrind ever ran.
#
#   * RUE-799: the Valgrind driver only discovered top-level examples and
#     discarded execution status. Nested roots, helper-module boundaries,
#     timeouts, signals, and failing curated self-checks therefore escaped its
#     contract.
#
# Each test runs a COPY of the real script in a throwaway sandbox with a fake
# `./buck2` (and, for scripts/rue, a fake scripts/rue-bin + fake compiler), so
# no real build runs. The fakes log their cwd/argv; we assert on exit status,
# surfaced stderr, and the directory the compiler was invoked from.
set -uo pipefail

# Root holding the real scripts. Under buck2 sh_test this is the materialized
# `:wrapper-script-inputs` filegroup (RUE_WRAPPER_ROOT); run directly from a
# checkout it defaults to the repo root (this script lives in scripts/).
if [ -n "${RUE_WRAPPER_ROOT:-}" ]; then
  SRC_ROOT="$RUE_WRAPPER_ROOT"
else
  SRC_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fi
FAILURES=0
TESTS=0

fail() { printf 'FAIL: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }
pass() { printf 'ok: %s\n' "$1"; }
# check <description> <0-if-ok|1-if-failed>
check() { TESTS=$((TESTS + 1)); if [ "$2" -eq 0 ]; then pass "$1"; else fail "$1"; fi; }

# ===========================================================================
# RUE-550 — resolver scripts must surface Buck failures, not swallow them
# ===========================================================================

# A failing `./buck2` must make rue-bin exit non-zero AND print Buck's stderr.
test_ruebin_build_failure_is_loud() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts"
  cp "$SRC_ROOT/scripts/rue-bin" "$sb/scripts/rue-bin"; chmod +x "$sb/scripts/rue-bin"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
echo "BUCK-DIAGNOSTIC: unknown target platform" >&2
exit 3
EOF
  chmod +x "$sb/buck2"

  local rc out
  out="$( "$sb/scripts/rue-bin" 2>&1 )"; rc=$?
  check "rue-bin: buck failure exits non-zero" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "rue-bin: buck stderr is surfaced" \
    "$(grep -q 'BUCK-DIAGNOSTIC' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# A successful build still prints ONLY the absolute binary path on stdout.
test_ruebin_success_prints_clean_path() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/fakebin"
  cp "$SRC_ROOT/scripts/rue-bin" "$sb/scripts/rue-bin"; chmod +x "$sb/scripts/rue-bin"
  printf '#!/bin/sh\ntrue\n' >"$sb/fakebin/compiler"; chmod +x "$sb/fakebin/compiler"
  # --show-output line: "<target> <repo-root-relative-path>"; plus build chatter
  # on stderr that must NOT leak into the captured stdout on success.
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
echo "build chatter that must stay on stderr" >&2
echo "root//crates/rue:rue fakebin/compiler"
EOF
  chmod +x "$sb/buck2"

  local rc out
  out="$( "$sb/scripts/rue-bin" 2>/dev/null )"; rc=$?
  check "rue-bin: success exits zero" "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "rue-bin: prints the absolute binary path only" \
    "$([ "$out" = "$sb/fakebin/compiler" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# fmt.sh delegates rustfmt's runtime environment to `buck2 run`; a failing
# invocation must remain loud.
test_fmt_build_failure_is_loud() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/crates"
  cp "$SRC_ROOT/fmt.sh" "$sb/fmt.sh"; chmod +x "$sb/fmt.sh"
  printf 'fn main() {}\n' >"$sb/crates/sample.rs"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
echo "BUCK-DIAGNOSTIC: rustfmt toolchain unavailable" >&2
exit 3
EOF
  chmod +x "$sb/buck2"

  local rc out
  out="$( bash "$sb/fmt.sh" 2>&1 )"; rc=$?
  check "fmt.sh: buck failure exits non-zero" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "fmt.sh: buck stderr is surfaced" \
    "$(grep -q 'BUCK-DIAGNOSTIC' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# fmt.sh must make one Buck RunInfo-based invocation and preserve every source
# path as one argument, including paths containing spaces.
test_fmt_uses_one_buck_run_and_preserves_paths() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/crates/with space"
  cp "$SRC_ROOT/fmt.sh" "$sb/fmt.sh"; chmod +x "$sb/fmt.sh"
  printf 'fn a() {}\n' >"$sb/crates/a.rs"
  printf 'fn b() {}\n' >"$sb/crates/with space/b.rs"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf 'invocation\n' >>"$BUCK_CALLS"
printf '%s\n' "$@" >"$BUCK_ARGS"
EOF
  chmod +x "$sb/buck2"

  local rc=0
  BUCK_CALLS="$sb/calls" BUCK_ARGS="$sb/args" \
    bash "$sb/fmt.sh" check >/dev/null 2>&1 || rc=$?

  check "fmt.sh: check succeeds through fake Buck" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "fmt.sh: invokes Buck exactly once" \
    "$([ "$(wc -l <"$sb/calls" 2>/dev/null | tr -d ' ')" = 1 ] && echo 0 || echo 1)"
  check "fmt.sh: uses the rustfmt RunInfo target" \
    "$(grep -Fxq 'run' "$sb/args" && grep -Fxq 'toolchains//rust:rustfmt' "$sb/args" && echo 0 || echo 1)"
  check "fmt.sh: preserves an ordinary source path" \
    "$(grep -Fxq "$sb/crates/a.rs" "$sb/args" && echo 0 || echo 1)"
  check "fmt.sh: preserves a source path containing spaces" \
    "$(grep -Fxq "$sb/crates/with space/b.rs" "$sb/args" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# ===========================================================================
# RUE-549 — scripts/rue run|exec resolve relative paths from the caller's cwd
# ===========================================================================

# Build a scripts/rue sandbox: a copy of the real script, a fake scripts/rue-bin
# that prints $FAKE_COMPILER, and a fake compiler that logs the cwd it ran in
# and fabricates its output binary (last argv) so `exec` can run it. Echoes the
# sandbox path.
make_rue_sandbox() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/work" "$sb/std" "$sb/examples" "$sb/probe"
  cp "$SRC_ROOT/scripts/rue" "$sb/scripts/rue"; chmod +x "$sb/scripts/rue"
  cat >"$sb/scripts/rue-bin" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$FAKE_COMPILER"
EOF
  chmod +x "$sb/scripts/rue-bin"
  cat >"$sb/compiler" <<'EOF'
#!/usr/bin/env bash
printf 'cwd=%s\n' "$PWD" >>"$COMPILE_LOG"
printf 'args=%s\n' "$*" >>"$COMPILE_LOG"
# Fabricate the output binary (the last argument) so a following exec can run it.
out="${!#}"
printf '#!/bin/sh\nexit 0\n' >"$out" 2>/dev/null || true
chmod +x "$out" 2>/dev/null || true
EOF
  chmod +x "$sb/compiler"
  echo 'fn main() -> i32 { 0 }' >"$sb/work/hello.rue"
  echo 'fn main() -> i32 { 0 }' >"$sb/examples/hello.rue"
  printf '%s\n' "$sb"
}

# `exec hello.rue` from a nested dir must invoke the compiler FROM that nested
# dir, so the relative source resolves there.
test_rue_exec_resolves_from_caller_cwd() {
  local sb; sb="$(make_rue_sandbox)"
  ( cd "$sb/work" && FAKE_COMPILER="$sb/compiler" COMPILE_LOG="$sb/compile.log" \
      "$sb/scripts/rue" exec hello.rue ) >/dev/null 2>&1
  local cwd_line
  # `grep -m1` reads the file directly. Piping into `head -1` would let head
  # close the pipe and, under `pipefail`, grep's EPIPE becomes the pipeline's
  # status -- the construct RUE-1011 already removed from this file (RUE-1155).
  cwd_line="$(grep -m1 '^cwd=' "$sb/compile.log" 2>/dev/null)"
  check "scripts/rue exec: compiler runs in the caller's cwd" \
    "$([ "$cwd_line" = "cwd=$sb/work" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# `run hello.rue -o out` from a nested dir must write the relative `-o` target
# into that nested dir (the caller's cwd), not the repo root.
test_rue_run_resolves_relative_output() {
  local sb; sb="$(make_rue_sandbox)"
  ( cd "$sb/work" && FAKE_COMPILER="$sb/compiler" COMPILE_LOG="$sb/compile.log" \
      "$sb/scripts/rue" run hello.rue -o out ) >/dev/null 2>&1
  local cwd_line
  # `grep -m1` reads the file directly. Piping into `head -1` would let head
  # close the pipe and, under `pipefail`, grep's EPIPE becomes the pipeline's
  # status -- the construct RUE-1011 already removed from this file (RUE-1155).
  cwd_line="$(grep -m1 '^cwd=' "$sb/compile.log" 2>/dev/null)"
  check "scripts/rue run: compiler runs in the caller's cwd" \
    "$([ "$cwd_line" = "cwd=$sb/work" ] && echo 0 || echo 1)"
  check "scripts/rue run: relative -o is written in the caller's cwd" \
    "$([ -f "$sb/work/out" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# ===========================================================================
# RUE-537 — filtered CLI wrappers anchor examples before the harness chdir
# ===========================================================================

# Install a fake Buck that behaves like the CLI harness at the relevant cwd
# boundary: once it receives RUE_EXAMPLES_DIR, it changes to a per-case
# directory and tries to read a repository example through that path.
install_cli_path_probe_buck() {
  local sb="$1"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
# RUE-1163: the corpus environment moved onto the //crates/rue-cli-tests:cli
# command_alias, so the wrappers hand buck2 nothing but the target and the
# filter. `$(location root//:examples)` expands to an absolute path there, which
# is what keeps a filtered examples case readable after the harness chdirs (the
# RUE-537/RUE-550 property); that is now Buck's guarantee rather than a
# `$repo_root/...` string built in shell, and //:cli-tests exercises it.
if [ "${1:-}" = "run" ]; then
  printf 'args=%s\n' "$*" >>"$CLI_LOG"
  exit "${FAKE_CLI_EXIT:-0}"
fi
exit 0
EOF
  chmod +x "$sb/buck2"
}

# `scripts/rue cli` must pass an absolute examples path that remains readable
# after the harness changes cwd, forward the filter, and preserve failures.
test_rue_cli_examples_survive_case_chdir() {
  local sb; sb="$(make_rue_sandbox)"
  install_cli_path_probe_buck "$sb"

  local rc=0
  ( cd "$sb/work" && FAKE_COMPILER="$sb/compiler" CLI_LOG="$sb/cli.log" \
      FAKE_PROBE_DIR="$sb/probe" \
      "$sb/scripts/rue" cli 'cli.examples::hello' ) >/dev/null 2>&1 || rc=$?
  check "scripts/rue cli: filtered run survives the harness cwd change" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "scripts/rue cli: delegates to the Buck entry point" \
    "$(grep -Fq 'run //crates/rue-cli-tests:cli' "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
  check "scripts/rue cli: sets no corpus environment of its own" \
    "$(! grep -Fq 'RUE_EXAMPLES_DIR' "$sb/scripts/rue" && echo 0 || echo 1)"
  check "scripts/rue cli: filter is forwarded" \
    "$(grep -Fq 'cli.examples::hello' "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"

  rc=0
  ( cd "$sb/work" && FAKE_COMPILER="$sb/compiler" CLI_LOG="$sb/cli.log" \
      FAKE_PROBE_DIR="$sb/probe" FAKE_CLI_EXIT=17 \
      "$sb/scripts/rue" cli 'cli.examples::hello' ) >/dev/null 2>&1 || rc=$?
  check "scripts/rue cli: harness failure is propagated" \
    "$([ "$rc" -eq 17 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# Canonical tier commands are intentionally thin: scripts/rue names the
# selection while test.sh owns execution and reads Buck's tier metadata. The
# required tier additionally fronts the clippy gate (RUE-1205): required CI
# runs ./clippy.sh as its own job next to the premerge Buck tier, so premerge
# (and the union tier, all) must run both for local green to predict CI green.
test_rue_named_test_tiers_delegate_to_testsh() {
  local sb; sb="$(make_rue_sandbox)"
  cat >"$sb/test.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${RUE_TEST_TIER:-unset}" >>"$TIER_LOG"
EOF
  chmod +x "$sb/test.sh"
  cat >"$sb/clippy.sh" <<'EOF'
#!/usr/bin/env bash
printf 'clippy\n' >>"$TIER_LOG"
exit "${FAKE_CLIPPY_EXIT:-0}"
EOF
  chmod +x "$sb/clippy.sh"

  local tier rc
  for tier in premerge slow stress all; do
    rc=0
    : >"$sb/tiers.log"
    (cd "$sb/work" && TIER_LOG="$sb/tiers.log" "$sb/scripts/rue" "$tier") \
      >/dev/null 2>&1 || rc=$?
    check "scripts/rue $tier: delegates the named tier to test.sh" \
      "$([ "$rc" -eq 0 ] && grep -Fxq "$tier" <<<"$(tail -1 "$sb/tiers.log")" && echo 0 || echo 1)"
    case "$tier" in
      premerge|all)
        check "scripts/rue $tier: runs the clippy gate before the tier" \
          "$([ "$(head -1 "$sb/tiers.log")" = clippy ] && echo 0 || echo 1)"
        ;;
      *)
        check "scripts/rue $tier: does not run the clippy gate" \
          "$(! grep -Fxq clippy "$sb/tiers.log" && echo 0 || echo 1)"
        ;;
    esac
  done

  # A clippy failure must fail the command and stop before the test tier runs.
  rc=0
  : >"$sb/tiers.log"
  (cd "$sb/work" && TIER_LOG="$sb/tiers.log" FAKE_CLIPPY_EXIT=13 \
    "$sb/scripts/rue" premerge) >/dev/null 2>&1 || rc=$?
  check "scripts/rue premerge: clippy failure is propagated" \
    "$([ "$rc" -eq 13 ] && echo 0 || echo 1)"
  check "scripts/rue premerge: clippy failure stops the tier run" \
    "$(! grep -Fxq premerge "$sb/tiers.log" && echo 0 || echo 1)"

  rc=0
  (cd "$sb/work" && TIER_LOG="$sb/tiers.log" "$sb/scripts/rue" slow unexpected) \
    >/dev/null 2>&1 || rc=$?
  check "scripts/rue slow: rejects case-filter arguments" \
    "$([ "$rc" -eq 2 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# Filtered `test.sh` reaches the same CLI path after its unit/spec/UI steps. Its
# absolute path must survive the cwd change, and its sentinel must agree with
# the propagated harness status.
test_testsh_cli_examples_survive_case_chdir() {
  local sb; sb="$(make_rue_sandbox)"
  install_cli_path_probe_buck "$sb"
  cp "$SRC_ROOT/test.sh" "$sb/test.sh"; chmod +x "$sb/test.sh"

  local rc=0 out
  out="$(cd "$sb/work" && FAKE_COMPILER="$sb/compiler" CLI_LOG="$sb/cli.log" \
      FAKE_PROBE_DIR="$sb/probe" \
      "$sb/test.sh" 'cli.examples::hello' 2>&1)" || rc=$?
  check "test.sh: filtered run survives the harness cwd change" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "test.sh: delegates to the Buck entry point" \
    "$(grep -Fq 'run //crates/rue-cli-tests:cli' "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
  check "test.sh: sets no corpus environment of its own" \
    "$(! grep -Fq 'RUE_EXAMPLES_DIR' "$sb/test.sh" && echo 0 || echo 1)"
  check "test.sh: CLI filter is forwarded" \
    "$(grep -Fq 'cli.examples::hello' "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
  check "test.sh: successful filtered run prints the passed sentinel" \
    "$(grep -Fxq '=== TEST SUITE: PASSED ===' <<< "$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb/work" && FAKE_COMPILER="$sb/compiler" CLI_LOG="$sb/cli.log" \
      FAKE_PROBE_DIR="$sb/probe" FAKE_CLI_EXIT=17 \
      "$sb/test.sh" 'cli.examples::hello' 2>&1)" || rc=$?
  check "test.sh: CLI harness failure is propagated" \
    "$([ "$rc" -eq 17 ] && echo 0 || echo 1)"
  check "test.sh: CLI harness failure prints the failed sentinel" \
    "$(grep -Fxq '=== TEST SUITE: FAILED (exit 17) ===' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# ===========================================================================
# RUE-590 — the sanitizer gives repository examples the bundled std library
# ===========================================================================

# Run a copy of the real sanitizer against a fake compiler and fake Valgrind.
# The fake compiler refuses every source unless RUE_STD_PATH names the expected
# std/ directory, then fabricates the output binary. This exercises
# every compiler invocation without needing a real compiler or Valgrind.
test_sanitizer_defaults_std_path() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/examples/calculator" "$sb/examples/std" \
    "$sb/std" "$sb/fakebin" "$sb/tmp"
  cp "$SRC_ROOT/scripts/run-sanitizer.sh" "$sb/scripts/run-sanitizer.sh"
  chmod +x "$sb/scripts/run-sanitizer.sh"
  printf 'const std = @import("std");\nfn main() -> i32 { 0 }\n' >"$sb/examples/std_probe.rue"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/calculator/main.rue"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/std/arraybuf_demo.rue"
  echo '// fake bundled standard library' >"$sb/std/_std.rue"

  cat >"$sb/compiler" <<'EOF'
#!/usr/bin/env bash
if [ "${RUE_STD_PATH:-}" != "$EXPECTED_STD" ]; then
  printf 'wrong RUE_STD_PATH: %s\n' "${RUE_STD_PATH:-<unset>}" >&2
  exit 88
fi
printf 'src=%s std=%s\n' "$1" "$RUE_STD_PATH" >>"$COMPILE_LOG"
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
[ -n "$out" ] || exit 89
printf '#!/bin/sh\nexit 0\n' >"$out"
chmod +x "$out"
EOF
  chmod +x "$sb/compiler"

  cat >"$sb/fakebin/valgrind" <<'EOF'
#!/usr/bin/env bash
log=""
for arg in "$@"; do
  case "$arg" in
    --log-file=*) log="${arg#--log-file=}" ;;
  esac
done
[ -n "$log" ] || exit 90
printf 'ERROR SUMMARY: 0 errors\n' >"$log"
EOF
  chmod +x "$sb/fakebin/valgrind"

  # run-sanitizer is a Linux/Valgrind driver, but this wrapper contract test
  # also runs on macOS, where GNU timeout is not installed by default.
  cat >"$sb/fakebin/timeout" <<'EOF'
#!/usr/bin/env bash
shift
exec "$@"
EOF
  chmod +x "$sb/fakebin/timeout"

  local rc=0
  env -u RUE_STD_PATH \
    PATH="$sb/fakebin:$PATH" \
    RUE_BINARY="$sb/compiler" \
    EXPECTED_STD="$sb/std" \
    COMPILE_LOG="$sb/compile.log" \
    TMPDIR="$sb/tmp" \
    "$sb/scripts/run-sanitizer.sh" >/dev/null 2>&1 || rc=$?

  check "run-sanitizer: std-importing examples receive repository std path" \
    "$([ "$rc" -eq 0 ] && grep -Fxq "src=$sb/examples/std_probe.rue std=$sb/std" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"

  mkdir -p "$sb/alternate-std"
  echo '// explicit alternate standard library' >"$sb/alternate-std/_std.rue"
  rc=0
  PATH="$sb/fakebin:$PATH" \
    RUE_BINARY="$sb/compiler" \
    RUE_STD_PATH="$sb/alternate-std" \
    EXPECTED_STD="$sb/alternate-std" \
    COMPILE_LOG="$sb/override-compile.log" \
    TMPDIR="$sb/tmp" \
    "$sb/scripts/run-sanitizer.sh" >/dev/null 2>&1 || rc=$?
  check "run-sanitizer: explicit std path override is preserved" \
    "$([ "$rc" -eq 0 ] && grep -Fxq "src=$sb/examples/std_probe.rue std=$sb/alternate-std" "$sb/override-compile.log" 2>/dev/null && echo 0 || echo 1)"
  rm -rf "$sb"
}

# Build a representative sanitizer sandbox. Its nested calculator directory
# has a root plus a helper that must never be compiled independently, while the
# std directory supplies the required nested ordinary-file sentinel.
make_sanitizer_sandbox() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/examples/calculator/lib" "$sb/examples/caldera" "$sb/examples/meridian" "$sb/examples/std" \
    "$sb/std" "$sb/fakebin" "$sb/tmp"
  cp "$SRC_ROOT/scripts/run-sanitizer.sh" "$sb/scripts/run-sanitizer.sh"
  chmod +x "$sb/scripts/run-sanitizer.sh"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/top.rue"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/calculator/main.rue"
  printf 'pub fn helper() -> i32 { 0 }\n' >"$sb/examples/calculator/lib/helper.rue"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/caldera/main.rue"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/meridian/main.rue"
  printf 'fn main() -> i32 { 0 }\n' >"$sb/examples/std/arraybuf_demo.rue"
  echo '// fake bundled standard library' >"$sb/std/_std.rue"

  cat >"$sb/compiler" <<'EOF'
#!/usr/bin/env bash
src="$1"
printf '%s\n' "$src" >>"$COMPILE_LOG"
out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then
    out="$2"
    shift 2
  else
    shift
  fi
done
[ -n "$out" ] || exit 89
case "$src" in
  */san_*.rue) status="${FAKE_CURATED_EXIT:-0}" ;;
  *) status="${FAKE_EXAMPLE_EXIT:-0}" ;;
esac
printf '#!/bin/sh\nexit %s\n' "$status" >"$out"
chmod +x "$out"
EOF
  chmod +x "$sb/compiler"

  cat >"$sb/fakebin/valgrind" <<'EOF'
#!/usr/bin/env bash
log=""
for arg in "$@"; do
  case "$arg" in
    --log-file=*) log="${arg#--log-file=}" ;;
  esac
done
[ -n "$log" ] || exit 90
case "${FAKE_VG_MODE:-clean}" in
  clean)
    printf 'ERROR SUMMARY: 0 errors\n' >"$log"
    "${!#}"
    ;;
  error)
    printf 'Invalid write of size 8\nERROR SUMMARY: 1 errors\n' >"$log"
    exit 125
    ;;
  signal)
    printf 'Process terminating with default action of signal 11 (SIGSEGV)\nERROR SUMMARY: 0 errors\n' >"$log"
    exit 139
    ;;
  *) exit 91 ;;
esac
EOF
  chmod +x "$sb/fakebin/valgrind"

  cat >"$sb/fakebin/timeout" <<'EOF'
#!/usr/bin/env bash
if [ "${FAKE_TIMEOUT:-0}" -ne 0 ]; then
  exit 124
fi
shift
exec "$@"
EOF
  chmod +x "$sb/fakebin/timeout"
  printf '%s\n' "$sb"
}

run_sanitizer_sandbox() {
  local sb="$1"
  PATH="$sb/fakebin:$PATH" \
    RUE_BINARY="$sb/compiler" \
    COMPILE_LOG="$sb/compile.log" \
    TMPDIR="$sb/tmp" \
    "$sb/scripts/run-sanitizer.sh"
}

test_sanitizer_recursive_discovery_contract() {
  local sb; sb="$(make_sanitizer_sandbox)"
  local rc=0
  run_sanitizer_sandbox "$sb" >/dev/null 2>&1 || rc=$?
  check "run-sanitizer: recursive representative corpus succeeds" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "run-sanitizer: discovers a nested root module" \
    "$(grep -Fxq "$sb/examples/calculator/main.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  check "run-sanitizer: discovers the nested std/ArrayBuf example" \
    "$(grep -Fxq "$sb/examples/std/arraybuf_demo.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  check "run-sanitizer: does not compile a root's helper module independently" \
    "$(! grep -Fxq "$sb/examples/calculator/lib/helper.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  check "run-sanitizer: default policy explicitly excludes Caldera" \
    "$(! grep -Fxq "$sb/examples/caldera/main.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  check "run-sanitizer: default policy explicitly excludes Meridian" \
    "$(! grep -Fxq "$sb/examples/meridian/main.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"; rc=0
  RUE_SANITIZER_LARGE_PROGRAMS=caldera run_sanitizer_sandbox "$sb" >/dev/null 2>&1 || rc=$?
  check "run-sanitizer: explicit Caldera selection includes its full root" \
    "$([ "$rc" -eq 0 ] && grep -Fxq "$sb/examples/caldera/main.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  check "run-sanitizer: selecting Caldera does not implicitly include Meridian" \
    "$(! grep -Fxq "$sb/examples/meridian/main.rue" "$sb/compile.log" 2>/dev/null && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_sanitizer_status_contracts() {
  local sb rc out

  sb="$(make_sanitizer_sandbox)"; rc=0
  FAKE_EXAMPLE_EXIT=124 run_sanitizer_sandbox "$sb" >/dev/null 2>&1 || rc=$?
  check "run-sanitizer: completed ordinary exit 124 is not mistaken for timeout" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"; rc=0
  out="$(FAKE_CURATED_EXIT=7 run_sanitizer_sandbox "$sb" 2>&1)" || rc=$?
  check "run-sanitizer: clean Memcheck plus nonzero curated self-check fails" \
    "$([ "$rc" -ne 0 ] && grep -q 'FAIL(program)' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"; rc=0
  out="$(FAKE_TIMEOUT=1 run_sanitizer_sandbox "$sb" 2>&1)" || rc=$?
  check "run-sanitizer: timeout status fails" \
    "$([ "$rc" -ne 0 ] && grep -q 'FAIL(timeout)' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"; rc=0
  out="$(FAKE_VG_MODE=signal run_sanitizer_sandbox "$sb" 2>&1)" || rc=$?
  check "run-sanitizer: fatal signal with a clean Memcheck summary fails" \
    "$([ "$rc" -ne 0 ] && grep -q 'FAIL(signal)' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"; rc=0
  out="$(FAKE_VG_MODE=error run_sanitizer_sandbox "$sb" 2>&1)" || rc=$?
  check "run-sanitizer: real Memcheck error fails" \
    "$([ "$rc" -ne 0 ] && grep -q 'FAIL(memcheck).*Valgrind status 125' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"
  rm -f "$sb/examples/calculator/main.rue"
  rc=0; out="$(run_sanitizer_sandbox "$sb" 2>&1)" || rc=$?
  check "run-sanitizer: missing nested root sentinel fails" \
    "$([ "$rc" -ne 0 ] && grep -q 'calculator/main.rue' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"

  sb="$(make_sanitizer_sandbox)"
  rm -rf "$sb/examples"; mkdir "$sb/examples"
  rc=0; out="$(run_sanitizer_sandbox "$sb" 2>&1)" || rc=$?
  check "run-sanitizer: empty example corpus fails loudly" \
    "$([ "$rc" -ne 0 ] && grep -q 'no .rue examples discovered' <<< "$out" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# ===========================================================================
# RUE-903 — scripts/rue unit runs ONE crate's tests with case-level filtering
# ===========================================================================

# Build a scripts/rue sandbox for the `unit` subcommand. Installs a copy of the
# real script, a couple of crate BUCK stubs (so crate-name validation passes),
# and a fake ./buck2 that logs every invocation's argv. On a --list preflight
# the fake emits $FAKE_LIST_OUT (a match line by default, so the preflight
# passes); the real run exits with $FAKE_RUN_EXIT. Echoes the sandbox path.
make_rue_unit_sandbox() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/crates/rue-parser" "$sb/crates/rue-compiler"
  cp "$SRC_ROOT/scripts/rue" "$sb/scripts/rue"; chmod +x "$sb/scripts/rue"
  printf '# stub\n' >"$sb/crates/rue-parser/BUCK"
  printf '# stub\n' >"$sb/crates/rue-compiler/BUCK"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$BUCK_LOG"
for a in "$@"; do
  if [ "$a" = "--list" ]; then
    printf '%s\n' "${FAKE_LIST_OUT-some::case: test}"
    exit 0
  fi
done
exit "${FAKE_RUN_EXIT:-0}"
EOF
  chmod +x "$sb/buck2"
  printf '%s\n' "$sb"
}

# The friendly crate name maps to //crates/<pkg>:<pkg>-test both with and
# without the rue- prefix, and libtest args are forwarded verbatim after `--`.
test_rue_unit_maps_crate_and_forwards_args() {
  local sb; sb="$(make_rue_unit_sandbox)"
  local rc=0
  ( cd "$sb" && BUCK_LOG="$sb/buck.log" \
      FAKE_LIST_OUT='parser::tests::diagnostic_corpus: test' \
      ./scripts/rue unit parser diagnostic_corpus --exact ) >/dev/null 2>&1 || rc=$?
  check "scripts/rue unit: prefix-less crate reaches the real run" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'run //crates/rue-parser:rue-parser-test -- diagnostic_corpus --exact' "$sb/buck.log" && echo 0 || echo 1)"

  : >"$sb/buck.log"; rc=0
  ( cd "$sb" && BUCK_LOG="$sb/buck.log" \
      FAKE_LIST_OUT='parser::tests::diagnostic_corpus: test' \
      ./scripts/rue unit rue-parser diagnostic_corpus ) >/dev/null 2>&1 || rc=$?
  check "scripts/rue unit: rue- prefixed crate resolves to the same target" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'run //crates/rue-parser:rue-parser-test -- diagnostic_corpus' "$sb/buck.log" && echo 0 || echo 1)"

  # The explicit <crate>:<target> form reaches an alternate target in the crate.
  : >"$sb/buck.log"; rc=0
  ( cd "$sb" && BUCK_LOG="$sb/buck.log" \
      FAKE_LIST_OUT='some::api_case: test' \
      ./scripts/rue unit compiler:rue-compiler-public-api-test ) >/dev/null 2>&1 || rc=$?
  check "scripts/rue unit: explicit crate:target form reaches the alternate target" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'run //crates/rue-compiler:rue-compiler-public-api-test --' "$sb/buck.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# A filter that selects nothing (libtest --list shows only its summary line,
# no `name: test` entries) must fail loudly and NEVER reach the real run.
test_rue_unit_zero_match_fails_loud() {
  local sb; sb="$(make_rue_unit_sandbox)"
  local rc=0 out
  out="$( cd "$sb" && BUCK_LOG="$sb/buck.log" \
      FAKE_LIST_OUT='0 tests, 0 benchmarks' \
      ./scripts/rue unit parser zzznomatch 2>&1 )" || rc=$?
  check "scripts/rue unit: empty selection exits non-zero" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "scripts/rue unit: empty selection prints a clear message" \
    "$(grep -q 'no tests matched' <<< "$out" && echo 0 || echo 1)"
  # Only the --list preflight should have run; no line lacking --list (the real
  # run) may appear in the log.
  check "scripts/rue unit: empty selection never runs the tests for real" \
    "$([ -z "$(grep -v -- '--list' "$sb/buck.log" 2>/dev/null)" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# A selected test that fails must propagate its non-zero exit code.
test_rue_unit_failing_test_propagates_exit() {
  local sb; sb="$(make_rue_unit_sandbox)"
  local rc=0
  ( cd "$sb" && BUCK_LOG="$sb/buck.log" \
      FAKE_LIST_OUT='parser::tests::flaky: test' FAKE_RUN_EXIT=101 \
      ./scripts/rue unit parser flaky ) >/dev/null 2>&1 || rc=$?
  check "scripts/rue unit: a failing selected test propagates its exit code" \
    "$([ "$rc" -eq 101 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# An unknown crate name fails with a clear message before any buck2 invocation.
test_rue_unit_unknown_crate_errors_cleanly() {
  local sb; sb="$(make_rue_unit_sandbox)"
  local rc=0 out
  out="$( cd "$sb" && BUCK_LOG="$sb/buck.log" \
      ./scripts/rue unit nosuchcrate foo 2>&1 )" || rc=$?
  check "scripts/rue unit: unknown crate exits non-zero" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "scripts/rue unit: unknown crate names the missing crate" \
    "$(grep -q "unknown crate 'nosuchcrate'" <<< "$out" && echo 0 || echo 1)"
  check "scripts/rue unit: unknown crate never invokes buck2" \
    "$([ ! -s "$sb/buck.log" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# CI timing summaries must preserve the wrapped command's output and exact exit
# status while aggregating every Buck command summary emitted by a multi-step
# wrapper such as test.sh.
test_ci_timed_preserves_status_and_summarizes_actions() {
  local sb; sb="$(mktemp -d)"
  cp "$SRC_ROOT/scripts/ci-timed" "$sb/ci-timed"; chmod +x "$sb/ci-timed"
  cat >"$sb/fake-command" <<'EOF'
#!/usr/bin/env bash
echo 'first line'
echo 'Commands: 10 (cached: 7, remote: 1, local: 2)'
echo 'Commands: 5 (cached: 3, remote: 1, local: 1)'
echo '✓ Pass: root//:spec-tests (1m12.250s)'
echo '✓ Pass: root//:ui-tests (7.5s)'
echo '✗ Fail: root//:cli-tests (2s)'
echo 'Skip: root//:stress-tests'
echo 'cli-tests: measured 37 cases in /tmp/timings.jsonl'
exit "${FAKE_EXIT:-0}"
EOF
  chmod +x "$sb/fake-command"

  local rc=0 out
  out="$(GITHUB_STEP_SUMMARY="$sb/summary" "$sb/ci-timed" "wrapper test" -- "$sb/fake-command" 2>&1)" || rc=$?
  check "ci-timed: wrapped output remains visible" \
    "$(grep -Fq 'first line' <<<"$out" && echo 0 || echo 1)"
  check "ci-timed: successful command exit is preserved" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "ci-timed: all Buck summaries are aggregated" \
    "$(grep -Fq '| passed |' "$sb/summary" && grep -Fq '| 2 | 15 | 10 | 2 | 3 |' "$sb/summary" && echo 0 || echo 1)"
  check "ci-timed: CLI case count is included" \
    "$(grep -Fq '| 37 | 2 | 15 |' "$sb/summary" && echo 0 || echo 1)"
  # 72.250 + 7.500 + 2.000, summed across minutes, fractional, and whole-second
  # spellings; the duration-less Skip line contributes nothing.
  check "ci-timed: opaque test-process time is separated from action counts" \
    "$(grep -Fq '| 81.750s |' "$sb/summary" && echo 0 || echo 1)"
  check "ci-timed: action-cache hit rate is reported" \
    "$(grep -Fq '| 66% |' "$sb/summary" && echo 0 || echo 1)"

  rc=0
  RUE_CI_REQUIRE_REMOTE_ACTIONS=1 "$sb/ci-timed" "remote wrapper" -- "$sb/fake-command" >/dev/null 2>&1 || rc=$?
  check "ci-timed: remote canary accepts remotely executed actions" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"

  cat >"$sb/local-command" <<'EOF'
#!/usr/bin/env bash
echo 'Commands: 4 (cached: 0, remote: 0, local: 4)'
EOF
  chmod +x "$sb/local-command"
  rc=0
  out="$(RUE_CI_REQUIRE_REMOTE_ACTIONS=1 "$sb/ci-timed" "local wrapper" -- "$sb/local-command" 2>&1)" || rc=$?
  check "ci-timed: remote canary rejects a local-only success" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "ci-timed: remote canary explains a local-only success" \
    "$(grep -Fq 'without any remotely executed Buck actions' <<<"$out" && echo 0 || echo 1)"

  : >"$sb/summary"; rc=0
  GITHUB_STEP_SUMMARY="$sb/summary" FAKE_EXIT=23 \
    "$sb/ci-timed" "failing wrapper" -- "$sb/fake-command" >/dev/null 2>&1 || rc=$?
  check "ci-timed: wrapped failure exit is preserved" \
    "$([ "$rc" -eq 23 ] && echo 0 || echo 1)"
  check "ci-timed: failed result is recorded" \
    "$(grep -Fq '| failed (23) |' "$sb/summary" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# The cache probe must distinguish a genuine same-run cold-to-warm conversion
# from an already-warm shared cache or a second build that did no better.
test_cache_probe_counter_validation() {
  local sb; sb="$(mktemp -d)"
  cp "$SRC_ROOT/scripts/check-cache-probe" "$sb/check-cache-probe"; chmod +x "$sb/check-cache-probe"

  printf '%s\n' 'Commands: 100 (cached: 10, remote: 0, local: 90)' >"$sb/cold.log"
  printf '%s\n' 'Commands: 100 (cached: 95, remote: 0, local: 5)' >"$sb/warm.log"
  local rc=0 out
  out="$("$sb/check-cache-probe" "$sb/cold.log" "$sb/warm.log" 2>&1)" || rc=$?
  check "cache probe: accepts a genuine cold-to-warm conversion" \
    "$([ "$rc" -eq 0 ] && grep -Fq 'Warm cache reused cold results' <<<"$out" && echo 0 || echo 1)"

  printf '%s\n' 'Commands: 100 (cached: 100, remote: 0, local: 0)' >"$sb/cold.log"
  rc=0
  out="$("$sb/check-cache-probe" "$sb/cold.log" "$sb/warm.log" 2>&1)" || rc=$?
  check "cache probe: rejects an already-warm cold phase" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'zero local actions' <<<"$out" && echo 0 || echo 1)"

  printf '%s\n' 'Commands: 100 (cached: 10, remote: 0, local: 90)' >"$sb/cold.log"
  printf '%s\n' 'Commands: 100 (cached: 10, remote: 0, local: 90)' >"$sb/warm.log"
  rc=0
  out="$("$sb/check-cache-probe" "$sb/cold.log" "$sb/warm.log" 2>&1)" || rc=$?
  check "cache probe: rejects a warm phase with no improvement" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'did not increase cache hits' <<<"$out" && echo 0 || echo 1)"

  printf '%s\n' 'no Buck summary' >"$sb/warm.log"
  rc=0
  out="$("$sb/check-cache-probe" "$sb/cold.log" "$sb/warm.log" 2>&1)" || rc=$?
  check "cache probe: rejects an unparseable summary" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'Could not parse' <<<"$out" && echo 0 || echo 1)"

  rm -rf "$sb"
}

# An explicit heavy-suite shard must still prove that Buck discovered and
# reported its assigned target; a green command with no result is a RUE-924
# false green even when the target pattern was explicit.
test_ci_heavy_suite_audits_its_target() {
  local sb; sb="$(mktemp -d)"
  cp "$SRC_ROOT/scripts/ci-heavy-suite" "$sb/ci-heavy-suite"; chmod +x "$sb/ci-heavy-suite"
  mkdir -p "$sb/scripts"
  cat >"$sb/scripts/cli-timeout-policy.py" <<'EOF'
#!/usr/bin/env bash
target=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--target" ]; then target="$2"; shift 2; else shift; fi
done
case "$target" in
  //:cli-tests) echo 3600 ;;
  //:cli-tests-slow) echo 7200 ;;
  //:cli-tests-shard-*) echo 1234 ;;
  *) exit 2 ;;
esac
EOF
  chmod +x "$sb/scripts/cli-timeout-policy.py"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "uquery" ]; then
  printf 'root%s\n' "${FAKE_LABELED_TARGET:-//:cli-tests}"
  exit 0
fi
if [ "$1" = "bxl" ]; then
  printf 'Rue test tiers valid\n'
  exit 0
fi
if [ "$1" = "test" ]; then
  if [ -n "${FAKE_CALL_LOG:-}" ]; then printf '%s\n' "$*" >>"$FAKE_CALL_LOG"; fi
  if [ "${FAKE_OMIT:-0}" != 1 ]; then printf 'Pass: root%s (0.1s)\n' "$2"; fi
  exit "${FAKE_EXIT:-0}"
fi
# RUE-1118: case timings are a declared output of the corpus action, fetched
# after the run rather than handed in as an executor --env path.
if [ "$1" = "build" ]; then
  if [ -n "${FAKE_CALL_LOG:-}" ]; then printf '%s\n' "$*" >>"$FAKE_CALL_LOG"; fi
  if [ "${FAKE_NO_TIMINGS:-0}" = 1 ]; then exit 1; fi
  out="$PWD/fake-case-timings.jsonl"
  printf '%s\n' '{"event":"rue_cli_case_timing","name":"fake","elapsed_s":0.1}' >"$out"
  printf '%s\n' "$out"
  exit 0
fi
exit 90
EOF
  chmod +x "$sb/buck2"

  local rc=0 out
  (cd "$sb" && ./ci-heavy-suite //:cli-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: labeled target with a result succeeds" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"

  # RUE-1159 flake detection only means anything if each repetition executes, so
  # a repetition run must defeat the corpus action's cache.
  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && RUE_CORRECTNESS_REPETITION=2 FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite //:cli-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: a correctness repetition runs cache-free" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--no-remote-cache' "$sb/calls.log" && grep -Fq 'RUE_CORRECTNESS_REPETITION=2' "$sb/calls.log" && echo 0 || echo 1)"

  # RUE-1118/RUE-1163: ci-heavy-suite carries no per-target executor timeouts at
  # all. Every corpus runs as a cacheable build action and the test executor
  # only asserts its stamp, so the outer bound belongs on the action — it is
  # each suite's timeout_seconds in BUCK. A stray `--timeout` here would bound
  # the wrong thing (a sub-second stamp check) and read as if the corpus were
  # still bounded, so pin that every target is invoked identically and bare.
  # This is the whole per-target contract now: the script has no `case` on the
  # target, so a new corpus needs no edit here.
  local heavy_target
  for heavy_target in \
    //:cli-tests \
    //:cli-tests-shard-1 \
    //:cli-tests-slow \
    //:spec-tests \
    //:ui-tests \
    //:frontend-diff-test \
    //:oracle-diff-generated-smoke \
    //:reproducible-programs \
    //crates/rue-oracle-diff:oracle-diff-test \
    //crates/rue-oracle-diff:oracle-diff-spec-test \
    //:tutorial-snippet-tests; do
    : >"$sb/calls.log"
    rc=0
    (cd "$sb" && FAKE_LABELED_TARGET="$heavy_target" FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite "$heavy_target") >/dev/null 2>&1 || rc=$?
    check "ci-heavy-suite: $heavy_target is invoked without an executor timeout" \
      "$([ "$rc" -eq 0 ] && grep -Fxq "test $heavy_target" "$sb/calls.log" && echo 0 || echo 1)"
  done

  # RUE-1158 under RUE-1118: per-case timings are a declared output of the
  # corpus action, so they are stored in the cache entry and materialize on a
  # hit too. ci-heavy-suite fetches the [timings] sub-target after the run and
  # copies it to the stable path ci.yml's upload step reads, rather than handing
  # the harness a per-run mktemp path that would change the digest every run.
  : >"$sb/calls.log"
  rc=0
  local timings_dir="$sb/runner-temp"
  mkdir -p "$timings_dir"
  (cd "$sb" && FAKE_LABELED_TARGET=//:cli-tests-shard-1 FAKE_CALL_LOG="$sb/calls.log" \
    RUNNER_TEMP="$timings_dir" ./ci-heavy-suite //:cli-tests-shard-1) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: a shard fetches timings from the action output" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--show-simple-output //:cli-tests-shard-1-action[timings]' "$sb/calls.log" && echo 0 || echo 1)"
  check "ci-heavy-suite: shard timings reach the upload path" \
    "$(grep -Fq '"event":"rue_cli_case_timing"' "$timings_dir/rue-cli-case-timings.jsonl" 2>/dev/null && echo 0 || echo 1)"
  check "ci-heavy-suite: no per-run timings path is handed to the executor" \
    "$(! grep -Fq 'RUE_CLI_CASE_TIMINGS=' "$sb/calls.log" && echo 0 || echo 1)"

  # An unsharded corpus emits no case timings, so it must not ask for them.
  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && FAKE_LABELED_TARGET=//:spec-tests FAKE_CALL_LOG="$sb/calls.log" \
    RUNNER_TEMP="$timings_dir" ./ci-heavy-suite //:spec-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: a non-shard corpus requests no timings" \
    "$([ "$rc" -eq 0 ] && ! grep -Fq 'timings' "$sb/calls.log" && echo 0 || echo 1)"

  # The artifact upload is if-no-files-found:ignore and the suite's own Buck
  # result is the required signal, so an absent timings output must not fail the
  # lane — it must only say so.
  rc=0
  out="$(cd "$sb" && FAKE_LABELED_TARGET=//:cli-tests-shard-1 FAKE_NO_TIMINGS=1 \
    RUNNER_TEMP="$timings_dir" ./ci-heavy-suite //:cli-tests-shard-1 2>&1)" || rc=$?
  check "ci-heavy-suite: a missing timings output is reported, not fatal" \
    "$([ "$rc" -eq 0 ] && grep -Fq 'no case timings produced' <<<"$out" && echo 0 || echo 1)"

  # RUE-1117: heavy suites are no longer only root-package targets. The label,
  # not the package path, decides what may run here.
  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && FAKE_LABELED_TARGET=//crates/rue-oracle-diff:oracle-diff-test \
    FAKE_CALL_LOG="$sb/calls.log" \
    ./ci-heavy-suite //crates/rue-oracle-diff:oracle-diff-test) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: a crate-package heavy suite is accepted" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'test //crates/rue-oracle-diff:oracle-diff-test' "$sb/calls.log" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && FAKE_LABELED_TARGET=//crates/rue-oracle-diff:oracle-diff-test \
    ./ci-heavy-suite //crates/rue-oracle-diff/... 2>&1)" || rc=$?
  check "ci-heavy-suite: a target pattern is still rejected as usage" \
    "$([ "$rc" -eq 2 ] && grep -Fq 'usage: scripts/ci-heavy-suite' <<<"$out" && echo 0 || echo 1)"

  # RUE-1163: a failing corpus fails through buck2's exit code. The old check
  # here asserted a grep for a `<Status>: root<target>` result line, which was
  # shell deciding whether the suite had really run; every corpus is now a build
  # action whose stamp its test asserts, so buck2 exiting 0 for a named target
  # means that corpus passed.
  rc=0
  out="$(cd "$sb" && FAKE_EXIT=1 ./ci-heavy-suite //:cli-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: a failing corpus propagates its exit code" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"

  rc=0
  (cd "$sb" && FAKE_EXIT=29 ./ci-heavy-suite //:cli-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: Buck failure exit is preserved" \
    "$([ "$rc" -eq 29 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# ===========================================================================
# RUE-924 — an unfiltered test.sh must FAIL LOUDLY when a corpus harness is
# silently omitted from the run's results, instead of reporting a green tally
# on a partial run (a served cache entry, a narrowed target pattern, or a
# platform gate once dropped //:cli-tests and let a Pass count hide five real
# CLI-case failures CI later caught).
# ===========================================================================

# RUE-1163: an unfiltered test.sh run is one `buck2 test` invocation. Selection
# is label filters buck2 evaluates; scheduling is buck2's, because each corpus
# action declares it needs the whole machine. These checks pin the filters and
# the exit-code contract — there is no membership query, no per-corpus loop, and
# no status aggregation left to test.
test_testsh_delegates_selection_to_buck() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts"
  cp "$SRC_ROOT/test.sh" "$sb/test.sh"
  chmod +x "$sb/test.sh"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
if [ -n "${FAKE_CALL_LOG:-}" ]; then printf '%s\n' "$*" >>"$FAKE_CALL_LOG"; fi
if [ "$1" = "bxl" ]; then printf 'Rue test tiers valid\n'; exit 0; fi
if [ "$1" = "test" ]; then
  printf 'Tests finished: Pass 1. Fail 0. Skip 0.\n'
  exit "${FAKE_BUCK_EXIT:-0}"
fi
exit 90
EOF
  chmod +x "$sb/buck2"

  local rc=0 out
  : >"$sb/calls.log"
  out="$(cd "$sb" && FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: unfiltered run reports success" \
    "$([ "$rc" -eq 0 ] && grep -Fxq '=== TEST SUITE: PASSED ===' <<< "$out" && echo 0 || echo 1)"
  check "test.sh: the tier-ownership gate runs before anything executes" \
    "$(grep -Fq 'bxl //test_tiers.bxl:validate' "$sb/calls.log" && echo 0 || echo 1)"
  check "test.sh: the whole run is a single buck2 test invocation" \
    "$([ "$(grep -c '^test ' "$sb/calls.log")" -eq 1 ] && echo 0 || echo 1)"
  check "test.sh: discovery stays broad" \
    "$(grep -Fq 'test //... toolchains//...' "$sb/calls.log" && echo 0 || echo 1)"
  check "test.sh: the CLI shards are excluded so the corpus runs once" \
    "$(grep -Fq -- '--exclude rue_cli_shard' "$sb/calls.log" && echo 0 || echo 1)"
  check "test.sh: heavy corpora are no longer excluded from the run" \
    "$(! grep -Fq -- '--exclude rue_heavy_suite' "$sb/calls.log" && echo 0 || echo 1)"
  check "test.sh: the standard tier leaves out opt-in stress tests" \
    "$(grep -Fq -- '--exclude rue_test_tier_stress' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && RUE_TEST_TIER=premerge FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: a named tier becomes a Buck label filter" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--include rue_test_tier_premerge' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && RUE_TEST_TIER=all FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: the union tier filters no tier out" \
    "$([ "$rc" -eq 0 ] && ! grep -Eq -- '--(include|exclude) rue_test_tier_' "$sb/calls.log" && echo 0 || echo 1)"

  # Required CI gives these corpora their own platform-corpus job; a local run
  # must still cover them.
  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && CI=true FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: required CI subtracts the dedicated-lane label" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--exclude rue_ci_dedicated_lane' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: a local run still covers the dedicated-lane corpora" \
    "$([ "$rc" -eq 0 ] && ! grep -Fq -- '--exclude rue_ci_dedicated_lane' "$sb/calls.log" && echo 0 || echo 1)"

  # The exit code is the verdict, and the sentinel agrees with it (RUE-579).
  rc=0
  out="$(cd "$sb" && FAKE_BUCK_EXIT=1 ./test.sh 2>&1)" || rc=$?
  check "test.sh: a failing run fails and says so" \
    "$([ "$rc" -ne 0 ] && grep -Fq '=== TEST SUITE: FAILED' <<<"$out" && echo 0 || echo 1)"

  rm -rf "$sb"
}

# --- run everything ---------------------------------------------------------

test_ruebin_build_failure_is_loud
test_ruebin_success_prints_clean_path
test_fmt_build_failure_is_loud
test_fmt_uses_one_buck_run_and_preserves_paths
test_rue_exec_resolves_from_caller_cwd
test_rue_run_resolves_relative_output
test_rue_cli_examples_survive_case_chdir
test_rue_named_test_tiers_delegate_to_testsh
test_testsh_cli_examples_survive_case_chdir
test_rue_unit_maps_crate_and_forwards_args
test_rue_unit_zero_match_fails_loud
test_rue_unit_failing_test_propagates_exit
test_rue_unit_unknown_crate_errors_cleanly
test_ci_timed_preserves_status_and_summarizes_actions
test_cache_probe_counter_validation
test_ci_heavy_suite_audits_its_target
test_testsh_delegates_selection_to_buck
test_sanitizer_defaults_std_path
test_sanitizer_recursive_discovery_contract
test_sanitizer_status_contracts

echo "--------------------------------------------------"
if [ "$FAILURES" -eq 0 ]; then
  echo "wrapper-script tests: all $TESTS checks passed"
  exit 0
else
  echo "wrapper-script tests: $FAILURES of $TESTS checks FAILED"
  exit 1
fi
