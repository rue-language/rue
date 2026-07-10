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
    "$(printf '%s' "$out" | grep -q 'BUCK-DIAGNOSTIC' && echo 0 || echo 1)"
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

# fmt.sh resolves rustfmt the same way; a failing `./buck2` must be loud too.
test_fmt_build_failure_is_loud() {
  local sb; sb="$(mktemp -d)"
  cp "$SRC_ROOT/fmt.sh" "$sb/fmt.sh"; chmod +x "$sb/fmt.sh"
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
    "$(printf '%s' "$out" | grep -q 'BUCK-DIAGNOSTIC' && echo 0 || echo 1)"
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
  cwd_line="$(grep '^cwd=' "$sb/compile.log" 2>/dev/null | head -1)"
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
  cwd_line="$(grep '^cwd=' "$sb/compile.log" 2>/dev/null | head -1)"
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
if [ -n "${RUE_EXAMPLES_DIR:-}" ]; then
  printf 'examples=%s\n' "$RUE_EXAMPLES_DIR" >>"$CLI_LOG"
  printf 'args=%s\n' "$*" >>"$CLI_LOG"
  cd "$FAKE_PROBE_DIR"
  [ -f "$RUE_EXAMPLES_DIR/hello.rue" ] || exit 91
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
  check "scripts/rue cli: examples path is anchored at the repository" \
    "$(grep -Fxq "examples=$sb/examples" "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
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
  check "test.sh: examples path is anchored at the repository" \
    "$(grep -Fxq "examples=$sb/examples" "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
  check "test.sh: CLI filter is forwarded" \
    "$(grep -Fq 'cli.examples::hello' "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
  check "test.sh: successful filtered run prints the passed sentinel" \
    "$(printf '%s\n' "$out" | grep -Fxq '=== TEST SUITE: PASSED ===' && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb/work" && FAKE_COMPILER="$sb/compiler" CLI_LOG="$sb/cli.log" \
      FAKE_PROBE_DIR="$sb/probe" FAKE_CLI_EXIT=17 \
      "$sb/test.sh" 'cli.examples::hello' 2>&1)" || rc=$?
  check "test.sh: CLI harness failure is propagated" \
    "$([ "$rc" -eq 17 ] && echo 0 || echo 1)"
  check "test.sh: CLI harness failure prints the failed sentinel" \
    "$(printf '%s\n' "$out" | grep -Fxq '=== TEST SUITE: FAILED (exit 17) ===' && echo 0 || echo 1)"
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
  mkdir -p "$sb/scripts" "$sb/examples" "$sb/std" "$sb/fakebin" "$sb/tmp"
  cp "$SRC_ROOT/scripts/run-sanitizer.sh" "$sb/scripts/run-sanitizer.sh"
  chmod +x "$sb/scripts/run-sanitizer.sh"
  printf 'const std = @import("std");\nfn main() -> i32 { 0 }\n' >"$sb/examples/std_probe.rue"
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

# --- run everything ---------------------------------------------------------

test_ruebin_build_failure_is_loud
test_ruebin_success_prints_clean_path
test_fmt_build_failure_is_loud
test_rue_exec_resolves_from_caller_cwd
test_rue_run_resolves_relative_output
test_rue_cli_examples_survive_case_chdir
test_testsh_cli_examples_survive_case_chdir
test_sanitizer_defaults_std_path

echo "--------------------------------------------------"
if [ "$FAILURES" -eq 0 ]; then
  echo "wrapper-script tests: all $TESTS checks passed"
  exit 0
else
  echo "wrapper-script tests: $FAILURES of $TESTS checks FAILED"
  exit 1
fi
