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

test_ruebin_session_bench_uses_canonical_resolver() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/fakebin"
  cp "$SRC_ROOT/scripts/rue-bin" "$sb/scripts/rue-bin"; chmod +x "$sb/scripts/rue-bin"
  printf '#!/bin/sh\ntrue\n' >"$sb/fakebin/session"; chmod +x "$sb/fakebin/session"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
echo "root//crates/rue-compiler-session-bench:rue-compiler-session-bench fakebin/session"
EOF
  chmod +x "$sb/buck2"

  local rc out
  out="$( "$sb/scripts/rue-bin" --session-bench 2>/dev/null )"; rc=$?
  check "rue-bin: session benchmark uses canonical absolute resolver" \
    "$([ "$rc" -eq 0 ] && [ "$out" = "$sb/fakebin/session" ] && echo 0 || echo 1)"
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

# Canonical tier commands are intentionally thin: scripts/rue names the
# selection while test.sh owns execution and reads Buck's tier metadata.
test_rue_named_test_tiers_delegate_to_testsh() {
  local sb; sb="$(make_rue_sandbox)"
  cat >"$sb/test.sh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${RUE_TEST_TIER:-unset}" >>"$TIER_LOG"
EOF
  chmod +x "$sb/test.sh"

  local tier rc
  for tier in premerge slow stress all; do
    rc=0
    (cd "$sb/work" && TIER_LOG="$sb/tiers.log" "$sb/scripts/rue" "$tier") \
      >/dev/null 2>&1 || rc=$?
    check "scripts/rue $tier: delegates the named tier to test.sh" \
      "$([ "$rc" -eq 0 ] && tail -1 "$sb/tiers.log" | grep -Fxq "$tier" && echo 0 || echo 1)"
  done

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
  check "test.sh: examples path is anchored at the repository" \
    "$(grep -Fxq "examples=$sb/examples" "$sb/cli.log" 2>/dev/null && echo 0 || echo 1)"
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
  for arg in "$@"; do
    case "$arg" in
      RUE_CLI_CASE_TIMINGS=*)
        timing_path="${arg#RUE_CLI_CASE_TIMINGS=}"
        printf '%s\n' '{"event":"rue_cli_case_timing","name":"fake","elapsed_s":0.1}' >"$timing_path"
        ;;
    esac
  done
  if [ "${FAKE_OMIT:-0}" != 1 ]; then printf 'Pass: root%s (0.1s)\n' "$2"; fi
  exit "${FAKE_EXIT:-0}"
fi
exit 90
EOF
  chmod +x "$sb/buck2"

  local rc=0 out
  (cd "$sb" && ./ci-heavy-suite //:cli-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: labeled target with a result succeeds" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"

  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite //:cli-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: premerge CLI corpus receives the extended executor timeout" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'test //:cli-tests -- --timeout 1800' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && FAKE_LABELED_TARGET=//:cli-tests-slow FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite //:cli-tests-slow) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: slow CLI corpus receives the extended executor timeout" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'test //:cli-tests-slow -- --timeout 1800' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && FAKE_LABELED_TARGET=//:cli-tests-shard-1 FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite //:cli-tests-shard-1) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: CLI shard receives the extended executor timeout" \
    "$([ "$rc" -eq 0 ] && grep -Eq '^test //:cli-tests-shard-1 -- --timeout 1200 --env RUE_CLI_CASE_TIMINGS=' "$sb/calls.log" && echo 0 || echo 1)"

  local other_target
  for other_target in \
    //:spec-tests \
    //:ui-tests \
    //:oracle-diff-generated-smoke \
    //:reproducible-programs \
    //:tutorial-snippet-tests; do
    : >"$sb/calls.log"
    rc=0
    (cd "$sb" && FAKE_LABELED_TARGET="$other_target" FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite "$other_target") >/dev/null 2>&1 || rc=$?
    check "ci-heavy-suite: $other_target retains the default executor timeout" \
      "$([ "$rc" -eq 0 ] && grep -Fxq "test $other_target" "$sb/calls.log" && echo 0 || echo 1)"
  done

  rc=0
  out="$(cd "$sb" && ./ci-heavy-suite //:spec-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: unlabeled shard target fails closed" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'not labeled rue_heavy_suite' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && FAKE_OMIT=1 ./ci-heavy-suite //:cli-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: omitted explicit result fails closed" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'produced no result' <<<"$out" && echo 0 || echo 1)"

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

# Drive a copy of the real test.sh (no filter) against a fake `./buck2` so the
# corpus-omission audit is exercised without a real (~10-minute) suite. The
# fake serves all three unfiltered-path invocations: the rue_heavy_suite
# uquery (returns FAKE_HEAVY_SUITES), the broad `test //...` pass (one
# unrelated pass line, exits FAKE_BUCK_EXIT), and each per-suite `test
# <target>` run (a result line only when the target is in FAKE_PASS_TARGETS).
test_testsh_unfiltered_audits_corpus_presence() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts"
  cp "$SRC_ROOT/test.sh" "$sb/test.sh"
  cp "$SRC_ROOT/scripts/ci-heavy-suite" "$sb/scripts/ci-heavy-suite"
  chmod +x "$sb/test.sh" "$sb/scripts/ci-heavy-suite"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "uquery" ]; then
  # RUE-1116: test.sh issues a second uquery for the rue_cli_shard set and
  # subtracts it from heavy-suite discovery. RUE-1157 adds a dedicated-suite
  # ownership query when required CI defers those tests to explicit jobs.
  case "$*" in
    *rue_cli_shard*) for t in ${FAKE_CLI_SHARDS:-}; do printf 'root%s\n' "$t"; done ;;
    *rue_dedicated_suite*) for t in ${FAKE_DEDICATED_SUITES:-}; do printf 'root%s\n' "$t"; done ;;
    *rue_test_tier_slow*) printf 'root//:cli-tests-slow\n' ;;
    *rue_test_tier_premerge*)
      for t in $FAKE_HEAVY_SUITES; do
        [ "$t" = "//:cli-tests-slow" ] || printf 'root%s\n' "$t"
      done
      ;;
    *) for t in $FAKE_HEAVY_SUITES; do printf 'root%s\n' "$t"; done ;;
  esac
  exit 0
fi
if [ "$1" = "bxl" ]; then
  printf 'Rue test tiers valid\n'
  exit 0
fi
if [ "$1" = "test" ]; then
  if [ -n "${FAKE_CALL_LOG:-}" ]; then printf '%s\n' "$*" >>"$FAKE_CALL_LOG"; fi
  tgt="$2"
  if [ "$tgt" = "//..." ]; then
    printf 'Pass: root//crates/rue-lexer:rue-lexer-test (0.1s)\n'
    printf 'Tests finished: Pass 1. Fail 0. Skip 0.\n'
    exit "${FAKE_BUCK_EXIT:-0}"
  fi
  for t in $FAKE_PASS_TARGETS; do
    if [ "$t" = "${tgt#root}" ]; then printf 'Pass: root%s (0.1s)\n' "$t"; fi
  done
  printf 'Tests finished: Pass 1. Fail 0. Skip 0.\n'
  exit 0
fi
exit 90
EOF
  chmod +x "$sb/buck2"

  local all="//:cli-tests //:cli-tests-slow //:large-example-caldera-canary //:large-example-meridian-canary //:spec-tests //:ui-tests //:oracle-diff-generated-smoke //:reproducible-programs //:tutorial-snippet-tests //:frontend-diff-test"

  # (1) Full corpus present + buck2 green -> test.sh reports success.
  local rc=0 out
  out="$(cd "$sb" && RUE_CI_DEFER_HEAVY_SUITES= RUE_FULL_SUITE_LOCK_HELD=1 FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" ./test.sh 2>&1)" || rc=$?
  check "test.sh: unfiltered run with the full corpus reports success" \
    "$([ "$rc" -eq 0 ] && grep -Fxq '=== TEST SUITE: PASSED ===' <<< "$out" && echo 0 || echo 1)"

  # The required-CI spelling selects premerge from canonical Buck metadata,
  # while slow selections audit their own real corpus target without claiming
  # the premerge omission sentinels executed.
  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && RUE_TEST_TIER=premerge RUE_CI_DEFER_HEAVY_SUITES= \
      RUE_FULL_SUITE_LOCK_HELD=1 FAKE_HEAVY_SUITES="$all" \
      FAKE_PASS_TARGETS="$all" FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: premerge selection uses the canonical Buck tier label" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--include rue_test_tier_premerge' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && RUE_TEST_TIER=slow RUE_CI_DEFER_HEAVY_SUITES= \
      RUE_FULL_SUITE_LOCK_HELD=1 FAKE_HEAVY_SUITES="$all" \
      FAKE_PASS_TARGETS='//:cli-tests-slow' FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: slow selection audits its real CLI corpus target" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--include rue_test_tier_slow' "$sb/calls.log" && grep -Fq 'test //:cli-tests-slow -- --timeout 1800' "$sb/calls.log" && echo 0 || echo 1)"

  # (2) A corpus harness OMITTED while every buck2 invocation still exits 0 is
  #     the RUE-924 false-green: it must become a hard failure naming the suite.
  local partial="//:cli-tests-slow //:large-example-caldera-canary //:large-example-meridian-canary //:spec-tests //:ui-tests //:oracle-diff-generated-smoke //:reproducible-programs //:tutorial-snippet-tests //:frontend-diff-test"
  rc=0
  out="$(cd "$sb" && RUE_CI_DEFER_HEAVY_SUITES= RUE_FULL_SUITE_LOCK_HELD=1 FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$partial" ./test.sh 2>&1)" || rc=$?
  check "test.sh: a silently omitted corpus harness fails the run" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "test.sh: the omission message names the missing harness" \
    "$(grep -Fq 'CORPUS OMITTED' <<< "$out" && grep -Fq '//:cli-tests' <<< "$out" && echo 0 || echo 1)"
  check "test.sh: an omitted-corpus run prints the failed sentinel" \
    "$(grep -Fq '=== TEST SUITE: FAILED' <<< "$out" && echo 0 || echo 1)"

  # (3) A genuine buck2 failure with the full corpus present is still
  #     propagated verbatim (the audit must not mask real failures).
  rc=0
  out="$(cd "$sb" && RUE_CI_DEFER_HEAVY_SUITES= RUE_FULL_SUITE_LOCK_HELD=1 FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" FAKE_BUCK_EXIT=17 ./test.sh 2>&1)" || rc=$?
  check "test.sh: unfiltered buck2 failure exit code is propagated" \
    "$([ "$rc" -eq 17 ] && echo 0 || echo 1)"

  # (4) Required CI may explicitly defer known live heavy targets to separate
  # jobs. The owning invocation must skip and stop auditing exactly those
  # targets while retaining every other corpus assertion.
  local owned="//:large-example-caldera-canary //:large-example-meridian-canary //:ui-tests //:oracle-diff-generated-smoke //:reproducible-programs //:tutorial-snippet-tests //:frontend-diff-test"
  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && CI=true RUE_TEST_TIER=premerge RUE_FULL_SUITE_LOCK_HELD=1 \
      RUE_CI_DEFER_HEAVY_SUITES='//:cli-tests //:spec-tests' \
      FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$owned" \
      FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: required CI may defer live heavy suites" \
    "$([ "$rc" -eq 0 ] && grep -Fq 'Deferring heavy suite root//:cli-tests' <<<"$out" && echo 0 || echo 1)"
  check "test.sh: deferred suites are not executed by the owning shard" \
    "$(! grep -Eq '^test (root)?//:(cli-tests|spec-tests)' "$sb/calls.log" && echo 0 || echo 1)"

  # (5) Dedicated pre-merge coverage may leave the broad CI pass only when the
  # workflow-owned set exactly matches Buck's live scheduling metadata.
  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && CI=true RUE_TEST_TIER=premerge RUE_FULL_SUITE_LOCK_HELD=1 \
      RUE_CI_DEFER_HEAVY_SUITES= \
      RUE_CI_DEFER_DEDICATED_SUITES='//crates/rue-compiler:scaling-matrix-test' \
      FAKE_DEDICATED_SUITES='//crates/rue-compiler:scaling-matrix-test' \
      FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" \
      FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: required CI may defer the exact live dedicated-suite set" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--exclude rue_dedicated_suite' "$sb/calls.log" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && CI=true RUE_TEST_TIER=premerge RUE_FULL_SUITE_LOCK_HELD=1 \
      RUE_CI_DEFER_HEAVY_SUITES= \
      RUE_CI_DEFER_DEDICATED_SUITES='//crates/rue-compiler:scaling-matrix-test' \
      FAKE_DEDICATED_SUITES='//crates/rue-compiler:scaling-matrix-test //:unowned-dedicated' \
      FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" ./test.sh 2>&1)" || rc=$?
  check "test.sh: a live dedicated target without a CI owner fails closed" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'has no explicit CI owner' <<<"$out" && echo 0 || echo 1)"

  # (6) A stale shard name or local attempt to suppress coverage fails closed.
  rc=0
  out="$(cd "$sb" && CI=true RUE_TEST_TIER=premerge RUE_FULL_SUITE_LOCK_HELD=1 \
      RUE_CI_DEFER_HEAVY_SUITES='//:missing-suite' \
      FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" ./test.sh 2>&1)" || rc=$?
  check "test.sh: unknown deferred CI suite fails closed" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'not labeled rue_heavy_suite' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && CI= RUE_FULL_SUITE_LOCK_HELD=1 \
      RUE_CI_DEFER_HEAVY_SUITES='//:cli-tests' \
      FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" ./test.sh 2>&1)" || rc=$?
  check "test.sh: local callers cannot defer corpus coverage" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'reserved for required CI' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && CI= RUE_FULL_SUITE_LOCK_HELD=1 \
      RUE_CI_DEFER_HEAVY_SUITES= \
      RUE_CI_DEFER_DEDICATED_SUITES='//crates/rue-compiler:scaling-matrix-test' \
      FAKE_DEDICATED_SUITES='//crates/rue-compiler:scaling-matrix-test' \
      FAKE_HEAVY_SUITES="$all" FAKE_PASS_TARGETS="$all" ./test.sh 2>&1)" || rc=$?
  check "test.sh: local callers cannot defer dedicated coverage" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'reserved for required CI' <<<"$out" && echo 0 || echo 1)"

  # (7) RUE-1116: the CI-only CLI shards are labeled rue_heavy_suite but must be
  #     excluded from a local full run, which covers their union once via the
  #     premerge //:cli-tests. The slow target independently owns its cases,
  #     and the standard run still succeeds with the full union present.
  local with_shards="$all //:cli-tests-shard-0 //:cli-tests-shard-1"
  : >"$sb/calls.log"; rc=0
  out="$(cd "$sb" && RUE_CI_DEFER_HEAVY_SUITES= RUE_FULL_SUITE_LOCK_HELD=1 \
      FAKE_HEAVY_SUITES="$with_shards" \
      FAKE_CLI_SHARDS='//:cli-tests-shard-0 //:cli-tests-shard-1' \
      FAKE_PASS_TARGETS="$all" FAKE_CALL_LOG="$sb/calls.log" ./test.sh 2>&1)" || rc=$?
  check "test.sh: local full run runs the premerge CLI target, not the shards" \
    "$([ "$rc" -eq 0 ] && ! grep -Eq '(^| )//:cli-tests-shard-' "$sb/calls.log" && echo 0 || echo 1)"

  rm -rf "$sb"
}

# --- run everything ---------------------------------------------------------

test_ruebin_build_failure_is_loud
test_ruebin_success_prints_clean_path
test_ruebin_session_bench_uses_canonical_resolver
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
test_testsh_unfiltered_audits_corpus_presence
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
