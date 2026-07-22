#!/usr/bin/env bash
# test-affected-targets.sh — regression tests for the deterministic logic of
# the affected-corpus selection (RUE-1119): the out-of-graph force-full matcher
# in scripts/affected-targets and the fail-open gate in scripts/ci-corpus-selected.
#
# These cover the parts that decide COVERAGE and must never regress: a path that
# must force a full run, a path that legitimately may be selected out, and the
# gate's fail-open behavior. The btd/buck2 runtime path is intentionally not
# exercised here — it fails open to a full run by construction — so this suite
# needs neither Buck nor network.
set -uo pipefail

if [ -n "${RUE_AFFECTED_SCRIPTS_ROOT:-}" ]; then
  SCRIPTS_DIR="$RUE_AFFECTED_SCRIPTS_ROOT/scripts"
else
  SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"
fi
# Invoke via `bash` so the suite does not depend on the execute bit surviving
# Buck resource materialization.
AFFECTED=(bash "$SCRIPTS_DIR/affected-targets")
GATE=(bash "$SCRIPTS_DIR/ci-corpus-selected")

FAILURES=0
TESTS=0

fail() { printf 'FAIL: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }
pass() { printf 'ok: %s\n' "$1"; }

# Assert that `path` forces a full run.
expect_full() {
  local path="$1"
  TESTS=$((TESTS + 1))
  if printf '%s\n' "$path" | "${AFFECTED[@]}" force-full-match >/dev/null; then
    pass "force-full: $path"
  else
    fail "force-full expected but not matched: $path"
  fi
}

# Assert that `path` does NOT force a full run (eligible for selection).
expect_selectable() {
  local path="$1"
  TESTS=$((TESTS + 1))
  if printf '%s\n' "$path" | "${AFFECTED[@]}" force-full-match >/dev/null; then
    fail "force-full unexpectedly matched (should be selectable): $path"
  else
    pass "selectable: $path"
  fi
}

# --- out-of-graph / graph-global changes MUST force a full run --------------

expect_full "BUCK"
expect_full "crates/rue-air/BUCK"
expect_full "toolchains/rust/defs.bzl"
expect_full "prelude/some.bzl"
expect_full ".buckconfig"
expect_full ".buckconfig.local.example"
expect_full ".buckroot"
expect_full ".github/workflows/ci.yml"
expect_full "toolchains/rust/BUCK"
expect_full "platforms/BUCK"
expect_full "constraints/BUCK"
expect_full "third-party/rust/Cargo.toml"
expect_full "rust-toolchain.toml"
expect_full "buck2"
expect_full "buck2-bin"
expect_full "reindeer"
expect_full "test.sh"
expect_full "scripts/ci-heavy-suite"
expect_full "scripts/ci-timed"
expect_full "scripts/ci-corpus-selected"
expect_full "scripts/affected-targets"
expect_full "scripts/install-btd"
expect_full "scripts/provision-build-cache"
expect_full "scripts/rue"
expect_full "scripts/rue-bin"

# --- peripheral / in-crate changes are eligible for selection --------------
# (a core-crate change still fans out to the whole suite via BTD's rdep
# closure; that decision belongs to btd, not to the force-full list.)

expect_selectable "crates/rue-air/src/lib.rs"
expect_selectable "crates/rue-spec/cases/4.2/example.rue"
expect_selectable "docs/process/logging.md"
expect_selectable "website/src/index.md"
expect_selectable "examples/hello/main.rue"
expect_selectable "README.md"
expect_selectable "scripts/generate-charts.py"

# A diff touching a mix of selectable and force-full paths forces a full run.
TESTS=$((TESTS + 1))
if printf '%s\n' "crates/rue-air/src/lib.rs" "toolchains/rust/defs.bzl" \
    | "${AFFECTED[@]}" force-full-match >/dev/null; then
  pass "force-full: mixed diff with one graph-global path"
else
  fail "force-full expected for mixed diff containing a .bzl change"
fi

# --- ci-corpus-selected gate fail-open behavior -----------------------------

gate() { # gate <full> <selected> <target>; echoes exit status
  RUE_AFFECTED_FULL="$1" RUE_AFFECTED_TARGETS="$2" "${GATE[@]}" "$3" >/dev/null 2>&1
  echo $?
}

check_gate() { # check_gate <desc> <expected-status> <full> <selected> <target>
  local desc="$1" expected="$2" got
  TESTS=$((TESTS + 1))
  got="$(gate "$3" "$4" "$5")"
  if [ "$got" = "$expected" ]; then
    pass "gate: $desc"
  else
    fail "gate: $desc (expected exit $expected, got $got)"
  fi
}

# full run => every corpus runs
check_gate "full=true runs corpus" 0 "true" "" "//:spec-tests"
# selective, target selected => runs
check_gate "selected target runs" 0 "false" "//:spec-tests //:cli-tests-caldera" "//:spec-tests"
# selective, target not selected => deselected
check_gate "unselected target deselected" 1 "false" "//:cli-tests-caldera" "//:spec-tests"
# selective, empty selection => everything deselected
check_gate "empty selection deselects" 1 "false" "" "//:spec-tests"
# fail-open: unset full runs
check_gate "unset full runs (fail-open)" 0 "" "" "//:spec-tests"
# fail-open: substring safety (shard-0 selected must not match shard-00)
check_gate "no substring false-positive" 1 "false" "//:cli-tests-shard-00" "//:cli-tests-shard-0"

# ---------------------------------------------------------------------------

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "=== affected-targets tests: PASSED ($TESTS checks) ==="
  exit 0
else
  echo "=== affected-targets tests: FAILED ($FAILURES/$TESTS checks) ===" >&2
  exit 1
fi
