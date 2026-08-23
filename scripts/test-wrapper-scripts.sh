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
# path as one argument, including paths containing spaces. It is write-mode
# only: check mode lives in the per-crate <name>-fmt-check tests (RUE-1153),
# so any argument is rejected rather than silently formatting in place when
# the caller expected a check.
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
    bash "$sb/fmt.sh" >/dev/null 2>&1 || rc=$?

  check "fmt.sh: write mode succeeds through fake Buck" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "fmt.sh: invokes Buck exactly once" \
    "$([ "$(wc -l <"$sb/calls" 2>/dev/null | tr -d ' ')" = 1 ] && echo 0 || echo 1)"
  check "fmt.sh: uses the rustfmt RunInfo target" \
    "$(grep -Fxq 'run' "$sb/args" && grep -Fxq 'toolchains//rust:rustfmt' "$sb/args" && echo 0 || echo 1)"
  check "fmt.sh: passes the repository rustfmt configuration" \
    "$(grep -Fxq -- '--config-path' "$sb/args" && grep -Fxq "$sb" "$sb/args" && echo 0 || echo 1)"
  check "fmt.sh: preserves an ordinary source path" \
    "$(grep -Fxq "$sb/crates/a.rs" "$sb/args" && echo 0 || echo 1)"
  check "fmt.sh: preserves a source path containing spaces" \
    "$(grep -Fxq "$sb/crates/with space/b.rs" "$sb/args" && echo 0 || echo 1)"

  # Any argument (including the retired `check`) must be refused with a
  # pointer to the per-crate gates, without touching Buck.
  rm -f "$sb/calls"
  rc=0
  local out
  out="$(BUCK_CALLS="$sb/calls" BUCK_ARGS="$sb/args" \
    bash "$sb/fmt.sh" check 2>&1)" || rc=$?
  check "fmt.sh: an argument is rejected with exit 2" \
    "$([ "$rc" -eq 2 ] && echo 0 || echo 1)"
  check "fmt.sh: the rejection names the per-crate fmt-check tests" \
    "$(grep -Fq -- '-fmt-check' <<<"$out" && echo 0 || echo 1)"
  check "fmt.sh: the rejection never invokes Buck" \
    "$([ ! -e "$sb/calls" ] && echo 0 || echo 1)"
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

# RUE-1739: the corpus wrappers must preserve the harness's non-zero result for
# a typoed explicit name filter. The direct subprocess target separately proves
# that the real binaries produce this diagnostic before scheduling any case.
install_zero_filter_buck() {
  local sb="$1"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = "run" ] && [[ "$*" = *rue1739_filter_must_not_match* ]]; then
  echo "no tests matched the given filter(s): rue1739_filter_must_not_match" >&2
  exit 101
fi
exit 0
EOF
  chmod +x "$sb/buck2"
}

test_rue_corpus_wrappers_reject_zero_filter() {
  local sb; sb="$(make_rue_sandbox)"
  install_zero_filter_buck "$sb"
  local kind rc out
  for kind in spec ui cli; do
    rc=0
    out="$(cd "$sb/work" && "$sb/scripts/rue" "$kind" rue1739_filter_must_not_match 2>&1)" || rc=$?
    check "scripts/rue $kind: zero-filter run exits non-zero" \
      "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
    check "scripts/rue $kind: zero-filter diagnostic is preserved" \
      "$(grep -q 'no tests matched' <<<"$out" && echo 0 || echo 1)"
  done
  rm -rf "$sb"
}

# crates/clippy-gate.sh is the decision step behind every per-crate
# <name>-clippy test (RUE-1153): the [clippy.txt] subtarget only captures
# diagnostics, so this script decides pass/fail. Clean and warning-only files
# pass, an `error:` line fails, and — the RUE-1152 lesson — a missing file is
# never certified as clean.
test_clippy_gate_reads_diagnostics_and_fails_closed() {
  local sb; sb="$(mktemp -d)"
  local gate="$SRC_ROOT/crates/clippy-gate.sh"

  local rc=0
  : >"$sb/clean.txt"
  bash "$gate" "$sb/clean.txt" >/dev/null 2>&1 || rc=$?
  check "clippy-gate.sh: empty diagnostics pass" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"

  # rustc prints some warnings unconditionally (e.g. -Ctarget-feature notes);
  # with deny_lints=["warnings"] every real finding is an error, so warnings
  # must not gate.
  rc=0
  printf 'warning: unstable feature note\n' >"$sb/warn.txt"
  bash "$gate" "$sb/warn.txt" >/dev/null 2>&1 || rc=$?
  check "clippy-gate.sh: warning-only diagnostics pass" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"

  # A real finding fails, and the diagnostics are surfaced — including when
  # clippy wrote them with ANSI colour codes.
  rc=0
  local out
  printf '\x1b[31merror\x1b[0m: used `len` on a `str`\n' >"$sb/lint.txt"
  out="$(bash "$gate" "$sb/lint.txt" 2>&1)" || rc=$?
  check "clippy-gate.sh: an error line fails the gate" \
    "$([ "$rc" -eq 1 ] && echo 0 || echo 1)"
  check "clippy-gate.sh: the violation is surfaced in the output" \
    "$(grep -Fq 'used `len` on a `str`' <<<"$out" && echo 0 || echo 1)"

  # A missing diagnostics file is not an empty one; refusing to certify what
  # cannot be read is the whole point of the gate (RUE-1152).
  rc=0
  out="$(bash "$gate" "$sb/nonexistent.txt" 2>&1)" || rc=$?
  check "clippy-gate.sh: a missing diagnostics file fails closed" \
    "$([ "$rc" -eq 1 ] && grep -Fq 'does not exist' <<<"$out" && echo 0 || echo 1)"

  rc=0
  bash "$gate" >/dev/null 2>&1 || rc=$?
  check "clippy-gate.sh: missing argument is a usage error" \
    "$([ "$rc" -eq 2 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# Canonical tier commands are intentionally thin: scripts/rue names the
# selection while test.sh owns execution and reads Buck's tier metadata. The
# fmt and clippy gates are per-crate premerge-tier tests emitted by the
# rue_crate/rue_binary macros (RUE-1153), so the tier run itself covers them —
# scripts/rue must not front any extra gate script of its own (the RUE-1205
# local-green-predicts-CI-green property now falls out of tier membership).
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
    : >"$sb/tiers.log"
    (cd "$sb/work" && TIER_LOG="$sb/tiers.log" "$sb/scripts/rue" "$tier") \
      >/dev/null 2>&1 || rc=$?
    check "scripts/rue $tier: delegates the named tier to test.sh" \
      "$([ "$rc" -eq 0 ] && grep -Fxq "$tier" <<<"$(tail -1 "$sb/tiers.log")" && echo 0 || echo 1)"
    check "scripts/rue $tier: the tier run is the only step" \
      "$([ "$(wc -l <"$sb/tiers.log" | tr -d ' ')" = 1 ] && echo 0 || echo 1)"
  done

  rc=0
  (cd "$sb/work" && TIER_LOG="$sb/tiers.log" "$sb/scripts/rue" slow unexpected) \
    >/dev/null 2>&1 || rc=$?
  check "scripts/rue slow: rejects case-filter arguments" \
    "$([ "$rc" -eq 2 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# `scripts/rue quick` is the fast loop the repository instructions tell every
# contributor to run during implementation, so its delegation must not break
# silently: the dispatcher names the selection, quick-test.sh owns the policy,
# and arguments pass through unchanged.
test_rue_quick_delegates_to_quick_testsh() {
  local sb; sb="$(make_rue_sandbox)"
  cat >"$sb/quick-test.sh" <<'EOF'
#!/usr/bin/env bash
printf 'ran\n' >>"$QUICK_CALLS"
printf '%s\n' "$@" >"$QUICK_ARGS"
EOF
  chmod +x "$sb/quick-test.sh"

  local rc=0
  (cd "$sb/work" && QUICK_CALLS="$sb/quick.calls" QUICK_ARGS="$sb/quick.args" \
    "$sb/scripts/rue" quick forwarded-arg) >/dev/null 2>&1 || rc=$?
  check "scripts/rue quick: delegates to quick-test.sh" \
    "$([ "$rc" -eq 0 ] && [ "$(wc -l <"$sb/quick.calls" 2>/dev/null | tr -d ' ')" = 1 ] && echo 0 || echo 1)"
  check "scripts/rue quick: forwards arguments unchanged" \
    "$(grep -Fxq 'forwarded-arg' "$sb/quick.args" 2>/dev/null && echo 0 || echo 1)"

  rc=0
  cat >"$sb/quick-test.sh" <<'EOF'
#!/usr/bin/env bash
exit 9
EOF
  chmod +x "$sb/quick-test.sh"
  (cd "$sb/work" && "$sb/scripts/rue" quick) >/dev/null 2>&1 || rc=$?
  check "scripts/rue quick: propagates a failing quick suite" \
    "$([ "$rc" -eq 9 ] && echo 0 || echo 1)"

  # Pin the real quick policy: premerge tier minus the not-quick and
  # dedicated-suite exclusions, scoped to first-party crates. A silently
  # narrowed or widened quick run breaks the fast-loop contract.
  check "quick-test.sh: selects the premerge tier" \
    "$(grep -Fq -- '--include rue_test_tier_premerge' "$SRC_ROOT/quick-test.sh" && echo 0 || echo 1)"
  check "quick-test.sh: keeps the not-quick and dedicated-suite exclusions" \
    "$(grep -Fq -- '--exclude rue_not_quick' "$SRC_ROOT/quick-test.sh" \
      && grep -Fq -- '--exclude rue_dedicated_suite' "$SRC_ROOT/quick-test.sh" && echo 0 || echo 1)"
  check "quick-test.sh: scopes to first-party crates" \
    "$(grep -Fq -- 'test //crates/...' "$SRC_ROOT/quick-test.sh" && echo 0 || echo 1)"
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

# A filtered test.sh run keeps the ordinary crate-unit coverage that quick-test
# uses, but must not pull in the slow/heavy oracle corpora or the opt-in stress
# ladder. Assert the complete Buck invocation, including its real //crates/...
# selection, so an exclusions-only change cannot pass while running zero units.
test_testsh_filtered_unit_selection_matches_quick_policy() {
  local sb; sb="$(mktemp -d)"
  cp "$SRC_ROOT/test.sh" "$sb/test.sh"; chmod +x "$sb/test.sh"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$FAKE_CALL_LOG"
exit 0
EOF
  chmod +x "$sb/buck2"

  local rc=0
  (cd "$sb" && FAKE_CALL_LOG="$sb/calls.log" ./test.sh 'parser::tests::case' \
    >/dev/null 2>&1) || rc=$?
  check "test.sh: filtered run succeeds with the fake Buck" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "test.sh: filtered unit step retains the crate-wide selection" \
    "$([ "$(grep -c '^test //crates/...' "$sb/calls.log" 2>/dev/null)" -eq 1 ] && echo 0 || echo 1)"
  check "test.sh: filtered unit step matches the canonical quick policy" \
    "$(grep -Fxq 'test //crates/... --include rue_test_tier_premerge --exclude rue_not_quick --exclude rue_dedicated_suite --always-exclude --ignore-tests-attribute' \
      "$sb/calls.log" && echo 0 || echo 1)"
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
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
# The invocation id is read from the environment at startup, before any argument
# is parsed and for every subcommand. A set-but-empty value is not "absent" — it
# is a malformed uuid, and the real binary dies on it. Unset is the only way to
# say "pick one for me". The fake used to accept the empty spelling, which is why
# a wrapper that exported one could not be caught here.
if [ -n "${BUCK_WRAPPER_UUID+set}" ] && [ -z "$BUCK_WRAPPER_UUID" ]; then
  printf 'Error: Parsing buck2 invocation id from env variable BUCK_WRAPPER_UUID\n\nCaused by:\n    invalid length: found 0\n' >&2
  exit 2
fi
if [ "$1" = "uquery" ]; then
  printf 'root%s\n' "${FAKE_LABELED_TARGET:-//:cli-tests}"
  exit 0
fi
if [ "$1" = "bxl" ]; then
  printf 'Rue test tiers valid\n'
  exit 0
fi
# RUE-1222. This fake used to accept ANY argument shape, which is how
# `buck2 test --env K=V` — rejected by the real binary at argument parsing,
# before `--` — stayed green here for months while every scheduled repetition
# run died in 18 seconds. Reproduce buck2's parser well enough that an argument
# the real binary refuses fails the test suite too.
subcommand="$1"
shift
reject_unknown() {
  printf "error: unexpected argument '%s' found\n\n  tip: to pass '%s' as a value, use '-- %s'\n" \
    "$1" "$1" "$1" >&2
  exit 3
}
parse_buck_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --) return 0 ;;
      -c) shift 2 || exit 3; continue ;;
      --no-remote-cache|--show-simple-output) shift; continue ;;
      --trace-id|--filter-category) shift 2 || exit 3; continue ;;
      --skip-cache-hits|--skip-remote-executions) shift; continue ;;
      -*) reject_unknown "$1" ;;
      *) shift; continue ;;
    esac
  done
}

# `what-ran` reports one tab-delimited row per command behind a header line with
# no tabs. Only `build` rows are Buck actions; a `test.run` row is a test
# execution, which never enters the cached/remote/local accounting at all. The
# fake serves rows only for the trace id the last `test` invocation recorded,
# because a bare `what-ran` reads whatever ran last in the isolation directory —
# the defect that made a failed run print a reassuring all-clear.
if [ "$subcommand" = "log" ]; then
  parse_buck_args "$@"
  requested=""
  while [ "$#" -gt 0 ]; do
    if [ "$1" = "--trace-id" ]; then requested="$2"; fi
    shift
  done
  recorded=""
  if [ -f "$PWD/fake-last-trace-id" ]; then recorded="$(cat "$PWD/fake-last-trace-id")"; fi
  if [ -z "$requested" ] || [ "$requested" != "$recorded" ]; then
    printf 'no event log for that trace id\n' >&2
    exit 1
  fi
  printf 'Showing commands from: buck2 test\n'
  if [ "${FAKE_LOCAL_ACTIONS:-0}" = 1 ]; then
    printf 'build\troot//:early (cfg#abc) (rustc early)\tlocal\tenv -C ...\n'
    printf 'build\troot//:thing (cfg#abc) (rue_corpus thing)\tlocal\tenv -C ...\n'
    printf 'test.run\tthing\tlocal\tthe stamp check\n'
  fi
  exit 0
fi
if [ "$subcommand" = "test" ]; then
  if [ -n "${FAKE_CALL_LOG:-}" ]; then printf 'test %s\n' "$*" >>"$FAKE_CALL_LOG"; fi
  parse_buck_args "$@"
  # An invocation that dies before starting writes no event log; `what-ran` then
  # answers about whatever ran previously in this isolation directory.
  if [ "${FAKE_NO_EVENT_LOG:-0}" = 1 ]; then
    rm -f "$PWD/fake-last-trace-id"
  else
    printf '%s' "${BUCK_WRAPPER_UUID:-}" >"$PWD/fake-last-trace-id"
  fi
  if [ "${FAKE_OMIT:-0}" != 1 ]; then printf 'Pass: root%s (0.1s)\n' "$1"; fi
  exit "${FAKE_EXIT:-0}"
fi
# RUE-1118: case timings are a declared output of the corpus action, fetched
# after the run rather than handed in as an executor --env path.
if [ "$subcommand" = "build" ]; then
  if [ -n "${FAKE_CALL_LOG:-}" ]; then printf 'build %s\n' "$*" >>"$FAKE_CALL_LOG"; fi
  parse_buck_args "$@"
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

  # RUE-1159 flake detection only means anything if each repetition executes.
  # RUE-1222: `--no-remote-cache` cannot deliver that on its own — it disables
  # the remote cache while buck2's local DICE state serves repetitions 2..N — so
  # the index rides a buckconfig that lands in the corpus action's env, making
  # each repetition a distinct digest buck2 must run. The old executor `--env`
  # spelling is what the real binary rejects outright, so assert its absence.
  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && RUE_CORRECTNESS_REPETITION=2 FAKE_CALL_LOG="$sb/calls.log" ./ci-heavy-suite //:cli-tests) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: a correctness repetition runs cache-free" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '--no-remote-cache' "$sb/calls.log" && echo 0 || echo 1)"
  check "ci-heavy-suite: the repetition index reaches the action as a buckconfig" \
    "$(grep -Fq -- '-c rue.corpus_repetition=2' "$sb/calls.log" && echo 0 || echo 1)"
  check "ci-heavy-suite: no executor --env is passed, which buck2 rejects" \
    "$(! grep -Fq -- '--env' "$sb/calls.log" && echo 0 || echo 1)"

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

  # RUE-1222: the scheduled sweep needs the repetition guarantee under its own
  # name, because it executes a corpus once instead of repeating it. Both buck2
  # invocations must agree about the cache: the timings fetch reuses the result
  # the test invocation just computed only if its execution configuration
  # matches, and a disagreement risks re-running the whole corpus.
  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && RUE_CORPUS_CACHE_FREE=1 FAKE_LABELED_TARGET=//:cli-tests-shard-1 \
    FAKE_CALL_LOG="$sb/calls.log" RUNNER_TEMP="$timings_dir" \
    ./ci-heavy-suite //:cli-tests-shard-1) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: a sweep run executes the corpus cache-free" \
    "$([ "$rc" -eq 0 ] && grep -Fxq 'test //:cli-tests-shard-1 --no-remote-cache' "$sb/calls.log" && echo 0 || echo 1)"
  check "ci-heavy-suite: the timings fetch shares the cache-free configuration" \
    "$(grep -Fq -- 'build --no-remote-cache --show-simple-output' "$sb/calls.log" && echo 0 || echo 1)"

  # The nonce must reach BOTH invocations. Naming the un-nonced action in the
  # timings fetch would either return another repetition's measurements or make
  # buck2 run the whole corpus a second time to produce them.
  : >"$sb/calls.log"
  rc=0
  (cd "$sb" && RUE_CORRECTNESS_REPETITION=4 FAKE_LABELED_TARGET=//:cli-tests-shard-1 \
    FAKE_CALL_LOG="$sb/calls.log" RUNNER_TEMP="$timings_dir" \
    ./ci-heavy-suite //:cli-tests-shard-1) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: the timings fetch names the same repetition's action" \
    "$([ "$rc" -eq 0 ] && [ "$(grep -Fc -- '-c rue.corpus_repetition=4' "$sb/calls.log")" = 2 ] && echo 0 || echo 1)"

  # RUE-1222: a cache hit replays the timings of whichever run wrote the entry,
  # so a cache-free repetition is the only source of freshly measured cost. Each
  # repetition must be able to keep its own file instead of overwriting the last.
  rc=0
  (cd "$sb" && FAKE_LABELED_TARGET=//:cli-tests-shard-1 \
    RUNNER_TEMP="$timings_dir" RUE_CLI_CASE_TIMINGS_DEST="$timings_dir/case-timings-3.jsonl" \
    ./ci-heavy-suite //:cli-tests-shard-1) >/dev/null 2>&1 || rc=$?
  check "ci-heavy-suite: the timings destination is overridable per repetition" \
    "$([ "$rc" -eq 0 ] && grep -Fq '"event":"rue_cli_case_timing"' "$timings_dir/case-timings-3.jsonl" && echo 0 || echo 1)"

  # RUE-1222: `Commands: ... local: 1` counts a cache miss without naming it,
  # which is why the linux-x64 corpus lanes' single non-cache-served action went
  # unidentified. Report the identity of every locally executed *action*; a
  # `test.run` row is a test execution and must not be reported as a miss.
  rc=0
  out="$(cd "$sb" && FAKE_LOCAL_ACTIONS=1 ./ci-heavy-suite //:cli-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: a locally executed action is named" \
    "$([ "$rc" -eq 0 ] && grep -Fq 'corpus lane: executed action root//:thing (cfg#abc) (rue_corpus thing)' <<<"$out" && echo 0 || echo 1)"
  check "ci-heavy-suite: a test execution is not reported as a cache miss" \
    "$([ "$(grep -Fc 'corpus lane: executed action' <<<"$out")" = 2 ] && ! grep -Fq 'stamp check' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && ./ci-heavy-suite //:cli-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: a fully cache-served lane says so" \
    "$([ "$rc" -eq 0 ] && grep -Fq 'corpus lane: every build action was served from cache' <<<"$out" && echo 0 || echo 1)"

  # RUE-1222 F1/F3 regression guards, both of which the old fake could not see.
  #
  # The fake now parses arguments the way buck2 does, so the argument order that
  # killed every scheduled run since RUE-1118 fails here instead of passing.
  rc=0
  out="$(cd "$sb" && ./buck2 test //:cli-tests --env RUE_CORRECTNESS_REPETITION=2 2>&1)" || rc=$?
  check "fake buck2: rejects --env before -- exactly as the real binary does" \
    "$([ "$rc" -eq 3 ] && grep -Fq "unexpected argument '--env' found" <<<"$out" && echo 0 || echo 1)"

  # A bare `what-ran` reads whatever ran last in the isolation directory, so a
  # run that wrote no event log would report a preceding command's actions as
  # its own — printing an all-clear for a lane that executed nothing. The report
  # must be pinned to this invocation's trace id.
  rc=0
  out="$(cd "$sb" && FAKE_LOCAL_ACTIONS=1 FAKE_NO_EVENT_LOG=1 FAKE_EXIT=3 \
    ./ci-heavy-suite //:cli-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: an invocation that logged nothing reports nothing" \
    "$([ "$rc" -eq 3 ] && ! grep -Fq 'served from cache' <<<"$out" && ! grep -Fq 'executed action' <<<"$out" && echo 0 || echo 1)"

  # The empty invocation id, which the real binary refuses before it parses an
  # argument. Asserted against the fake directly, the way the `--env` guard above
  # is: the wrapper test below is only meaningful if this divergence is visible.
  rc=0
  out="$(cd "$sb" && BUCK_WRAPPER_UUID="" ./buck2 test //:cli-tests 2>&1)" || rc=$?
  check "fake buck2: refuses an empty BUCK_WRAPPER_UUID as the real binary does" \
    "$([ "$rc" -eq 2 ] && grep -Fq 'invalid length: found 0' <<<"$out" && echo 0 || echo 1)"

  # ...so a host with no usable uuid source must lose the report and keep the
  # run. Exporting the variable empty made that branch fatal instead.
  #
  # The shim stands in for both arms of the fallback chain failing: what the code
  # under test sees either way is an empty trace id, and a shim is the only
  # spelling of that which reproduces on Linux, where the
  # /proc/sys/kernel/random/uuid fallback would otherwise succeed.
  mkdir -p "$sb/nouuid"
  cat >"$sb/nouuid/uuidgen" <<'EOF'
#!/bin/sh
exit 1
EOF
  chmod +x "$sb/nouuid/uuidgen"
  : >"$sb/calls.log"
  rc=0
  out="$(cd "$sb" && PATH="$sb/nouuid:$PATH" FAKE_CALL_LOG="$sb/calls.log" \
    ./ci-heavy-suite //:cli-tests 2>&1)" || rc=$?
  check "ci-heavy-suite: no uuid source costs the what-ran report, not the run" \
    "$([ "$rc" -eq 0 ] && grep -Fq -- '//:cli-tests' "$sb/calls.log" && echo 0 || echo 1)"
  check "ci-heavy-suite: and it claims nothing about caching it cannot know" \
    "$(! grep -Fq 'served from cache' <<<"$out" && ! grep -Fq 'executed action' <<<"$out" && echo 0 || echo 1)"

  rm -rf "$sb"
}

# ===========================================================================
# RUE-1222 — the corpus inventory that drives the scheduled cache-free sweep.
#
# The sweep exists because an undeclared action input is a false pass: the cache
# serves the previous tree's stamp and the corpus reports success having run
# nothing. That guarantee is only worth as much as the inventory behind it, so
# the list comes from the Buck graph and an inventory that cannot be read must
# fail rather than report a vacuous clean sweep.
# ===========================================================================

test_ci_corpus_inventory_is_graph_derived_and_fails_closed() {
  local sb; sb="$(mktemp -d)"
  mkdir -p "$sb/scripts"
  cp "$SRC_ROOT/scripts/ci-corpus-inventory" "$sb/scripts/ci-corpus-inventory"
  chmod +x "$sb/scripts/ci-corpus-inventory"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
if [ "$1" = "uquery" ]; then
  if [ "${FAKE_QUERY_EXIT:-0}" != 0 ]; then
    printf 'buck2 exploded\n' >&2
    exit "$FAKE_QUERY_EXIT"
  fi
  # RUE-1222: the inventory cross-checks the corpus-action set against the
  # rue_heavy_suite label set and resolves --exclude-label through the graph as
  # well, so the fake must answer all three queries apart.
  case "$2" in
    *rue_heavy_suite*)
      if [ "${FAKE_HEAVY_QUERY_EXIT:-0}" != 0 ]; then exit "$FAKE_HEAVY_QUERY_EXIT"; fi
      # An empty graph has no heavy suites either; that case exercises the
      # vacuous-sweep guard rather than the cross-check.
      if [ "${FAKE_EMPTY:-0}" = 1 ]; then exit 0; fi
      printf 'root//:cli-tests\n'
      printf 'root//:spec-tests\n'
      if [ "${FAKE_UNCONVERTED_HEAVY:-0}" = 1 ]; then printf 'root//:tutorial-snippet-tests\n'; fi
      exit 0
      ;;
    *rue_cli_shard*)
      if [ "${FAKE_LABEL_QUERY_EXIT:-0}" != 0 ]; then exit "$FAKE_LABEL_QUERY_EXIT"; fi
      printf 'root//:cli-tests-shard-0\n'
      printf 'root//:cli-tests-shard-1\n'
      exit 0
      ;;
    *rue_no_such_label*)
      exit 0
      ;;
  esac
  if [ "${FAKE_EMPTY:-0}" = 1 ]; then exit 0; fi
  if [ "${FAKE_BAD_LABEL:-0}" = 1 ]; then printf 'root//:not-a-corpus\n'; exit 0; fi
  printf 'root//:cli-tests-action\n'
  printf 'root//:cli-tests-shard-0-action\n'
  printf 'root//:cli-tests-shard-1-action\n'
  # A corpus named like a shard but not labeled one. It stands for the way a
  # prefix exclusion loses a corpus: `//:cli-tests-shard-*` would swallow it
  # after the completeness cross-check had already passed, so it would leave the
  # sweep with nothing anywhere reporting an error.
  printf 'root//:cli-tests-shard-weights-smoke-action\n'
  printf 'root//:spec-tests-action\n'
  printf 'root//crates/rue-oracle-diff:oracle-diff-test-action\n'
  exit 0
fi
exit 90
EOF
  chmod +x "$sb/buck2"

  local rc=0 out
  out="$(cd "$sb" && scripts/ci-corpus-inventory 2>&1)" || rc=$?
  check "ci-corpus-inventory: every corpus action maps back to its suite" \
    "$([ "$rc" -eq 0 ] && [ "$out" = '//:cli-tests
//:cli-tests-shard-0
//:cli-tests-shard-1
//:cli-tests-shard-weights-smoke
//:spec-tests
//crates/rue-oracle-diff:oracle-diff-test' ] && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && scripts/ci-corpus-inventory --json \
    --exclude '//:cli-tests' --exclude-label rue_cli_shard 2>&1)" || rc=$?
  check "ci-corpus-inventory: --json emits a matrix-ready array with exclusions applied" \
    "$([ "$rc" -eq 0 ] && [ "$out" = '["//:cli-tests-shard-weights-smoke","//:spec-tests","//crates/rue-oracle-diff:oracle-diff-test"]' ] && echo 0 || echo 1)"

  # The finding this replaced a glob to fix. `//:cli-tests-shard-*` matched the
  # decoy too, and it was applied AFTER the heavy-suite cross-check, so the
  # corpus left the sweep with every check still green. Resolving the real
  # shards through their label keeps a look-alike in.
  check "ci-corpus-inventory: a corpus merely named like a shard stays in the sweep" \
    "$(grep -Fq 'cli-tests-shard-weights-smoke' <<<"$out" && echo 0 || echo 1)"

  # ...and the spelling that could reintroduce it is refused outright rather
  # than silently matching one literal target.
  rc=0
  out="$(cd "$sb" && scripts/ci-corpus-inventory --exclude '//:cli-tests-shard-*' 2>&1)" || rc=$?
  check "ci-corpus-inventory: a glob exclusion is rejected, not honored" \
    "$([ "$rc" -eq 2 ] && grep -Fq 'exact target label' <<<"$out" && echo 0 || echo 1)"

  # A label naming nothing excludes nothing: over-sweeping is safe, and failing
  # here would make removing a label break an unrelated workflow.
  rc=0
  out="$(cd "$sb" && scripts/ci-corpus-inventory --exclude-label rue_no_such_label 2>&1)" || rc=$?
  check "ci-corpus-inventory: a label that resolves to nothing sweeps everything" \
    "$([ "$rc" -eq 0 ] && [ "$(wc -l <<<"$out" | tr -d ' ')" = 6 ] && echo 0 || echo 1)"

  # But a label set that cannot be READ is the under-sweeping direction again.
  rc=0
  out="$(cd "$sb" && FAKE_LABEL_QUERY_EXIT=5 scripts/ci-corpus-inventory \
    --exclude-label rue_cli_shard 2>&1)" || rc=$?
  check "ci-corpus-inventory: an unreadable exclusion label fails closed" \
    "$([ "$rc" -ne 0 ] && grep -Fq "resolve an exclusion" <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && FAKE_QUERY_EXIT=3 scripts/ci-corpus-inventory 2>&1)" || rc=$?
  check "ci-corpus-inventory: a failed query fails closed" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'could not query the corpus inventory' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && FAKE_EMPTY=1 scripts/ci-corpus-inventory 2>&1)" || rc=$?
  check "ci-corpus-inventory: an empty inventory fails instead of sweeping nothing" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'refusing to report a vacuous sweep' <<<"$out" && echo 0 || echo 1)"

  # Excluding the whole graph one target at a time is still a vacuous sweep, and
  # has to fail for the same reason an unreadable one does. Spelled out in full
  # now that there is no wildcard to say it in one argument.
  rc=0
  out="$(cd "$sb" && scripts/ci-corpus-inventory \
    --exclude '//:cli-tests' \
    --exclude '//:cli-tests-shard-weights-smoke' \
    --exclude '//:spec-tests' \
    --exclude '//crates/rue-oracle-diff:oracle-diff-test' \
    --exclude-label rue_cli_shard 2>&1)" || rc=$?
  check "ci-corpus-inventory: excluding everything is an empty inventory too" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'refusing to report a vacuous sweep' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && FAKE_BAD_LABEL=1 scripts/ci-corpus-inventory 2>&1)" || rc=$?
  check "ci-corpus-inventory: an unrecognized action label is an error, not a silent drop" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'unexpected corpus action label' <<<"$out" && echo 0 || echo 1)"

  # RUE-1222: "every corpus" has to be enforced, not conventional. A heavy suite
  # that is not a converted corpus would be absent from the sweep while the
  # sweep still reported success, which is the failure this whole lane exists to
  # prevent one level down.
  rc=0
  out="$(cd "$sb" && FAKE_UNCONVERTED_HEAVY=1 scripts/ci-corpus-inventory 2>&1)" || rc=$?
  check "ci-corpus-inventory: a heavy suite with no corpus action fails the inventory" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'heavy suite(s) with no corpus action' <<<"$out" \
      && grep -Fq '//:tutorial-snippet-tests' <<<"$out" && echo 0 || echo 1)"

  rc=0
  out="$(cd "$sb" && FAKE_HEAVY_QUERY_EXIT=4 scripts/ci-corpus-inventory 2>&1)" || rc=$?
  check "ci-corpus-inventory: an unreadable heavy-suite set fails closed too" \
    "$([ "$rc" -ne 0 ] && grep -Fq 'cross-check' <<<"$out" && echo 0 || echo 1)"

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
test_rue_corpus_wrappers_reject_zero_filter
test_clippy_gate_reads_diagnostics_and_fails_closed
test_rue_named_test_tiers_delegate_to_testsh
test_rue_quick_delegates_to_quick_testsh
test_testsh_cli_examples_survive_case_chdir
test_testsh_filtered_unit_selection_matches_quick_policy
test_rue_unit_maps_crate_and_forwards_args
test_rue_unit_zero_match_fails_loud
test_rue_unit_failing_test_propagates_exit
test_rue_unit_unknown_crate_errors_cleanly
test_ci_timed_preserves_status_and_summarizes_actions
test_cache_probe_counter_validation
test_ci_heavy_suite_audits_its_target
test_ci_corpus_inventory_is_graph_derived_and_fails_closed
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
