#!/usr/bin/env bash
# test-affected-targets.sh — regression tests for the deterministic logic of
# the affected-corpus selection (RUE-1119): the out-of-graph force-full matcher
# in scripts/affected-targets and the fail-open gate in scripts/ci-corpus-selected.
#
# These cover the parts that decide COVERAGE and must never regress: a path that
# must force a full run, a path that legitimately may be selected out, strict
# BTD decoding, the gate's fail-open behavior, and one selective end-to-end
# decision with local BTD/Buck stubs. The suite needs neither network nor Buck.
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
DECISION=(bash "$SCRIPTS_DIR/ci-corpus-decision")
PARSER=(python3 "$SCRIPTS_DIR/parse-btd-impacted.py")

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
expect_full "btd"
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
expect_selectable "website/build.sh"

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
check_gate "selected target runs" 0 "false" "//:spec-tests //:cli-tests-shard-0" "//:spec-tests"
# selective, target not selected => deselected
check_gate "unselected target deselected" 1 "false" "//:cli-tests-shard-0" "//:spec-tests"
# selective, empty selection => everything deselected
check_gate "empty selection deselects" 1 "false" "" "//:spec-tests"
# fail-open: unset full runs
check_gate "unset full runs (fail-open)" 0 "" "" "//:spec-tests"
# fail-open: substring safety (shard-0 selected must not match shard-00)
check_gate "unselected valid target deselects" 1 "false" "//:cli-tests-shard-1" "//:cli-tests-shard-0"
# unknown matrix entry fails open rather than being dropped
check_gate "unknown matrix target runs" 0 "false" "" "//:future-corpus"
# malformed selected output must not look like an intentional deselection
check_gate "unknown selected output is an error" 2 "false" "//:future-corpus" "//:spec-tests"

# The workflow-facing adapter must reserve `run=false` for the gate's explicit
# deselection status. A gate crash or missing executable runs the corpus.
decision_root="$(mktemp -d)"
printf '#!/usr/bin/env bash\nexit 1\n' >"$decision_root/deselect"
printf '#!/usr/bin/env bash\nexit 2\n' >"$decision_root/crash"
chmod +x "$decision_root/deselect" "$decision_root/crash"
check_decision() { # check_decision <description> <gate> <expected-output>
  local desc="$1" gate="$2" expected="$3" output="$decision_root/output"
  TESTS=$((TESTS + 1))
  : >"$output"
  if RUE_AFFECTED_GATE="$gate" GITHUB_OUTPUT="$output" "${DECISION[@]}" "//:spec-tests" >/dev/null 2>&1 && \
      grep -Fxq "run=$expected" "$output"; then
    pass "decision: $desc"
  else
    fail "decision: $desc"
  fi
}
check_decision "exit 1 intentionally deselects" "$decision_root/deselect" false
check_decision "crashing gate runs" "$decision_root/crash" true
check_decision "missing gate runs" "$decision_root/missing" true
rm -rf "$decision_root"

# --- strict BTD JSON decoding -----------------------------------------------

check_parser_ok() { # check_parser_ok <description> <input> <expected>
  local desc="$1" input="$2" expected="$3" got
  TESTS=$((TESTS + 1))
  if got="$(printf '%b' "$input" | "${PARSER[@]}")" && [ "$got" = "$expected" ]; then
    pass "parser: $desc"
  else
    fail "parser: $desc"
  fi
}

check_parser_bad() { # check_parser_bad <description> <input>
  local desc="$1" input="$2"
  TESTS=$((TESTS + 1))
  if printf '%b' "$input" | "${PARSER[@]}" >/dev/null 2>&1; then
    fail "parser: $desc (unexpected success)"
  else
    pass "parser: $desc"
  fi
}

check_parser_ok "empty stream is valid" "" ""
check_parser_ok "normalizes root cell" '{"target":"root//:spec-tests"}\n' "//:spec-tests"
check_parser_bad "partially malformed stream fails" '{"target":"root//:spec-tests"}\nnot-json\n'
check_parser_bad "wholly malformed stream fails" 'not-json\n'
check_parser_bad "missing target fails" '{}\n'
check_parser_bad "non-string target fails" '{"target":7}\n'

# Git's BTD input is status records, while force-full matches paths alone.
# These prove the projection preserves all A/M/D records and rejects an unknown
# status instead of accidentally running selectively.
TESTS=$((TESTS + 1))
if got="$(printf 'A\tadded.rue\nM\tchanged.rue\nD\tdeleted.rue\n' | "${AFFECTED[@]}" status-paths)" && \
    [ "$got" = $'added.rue\nchanged.rue\ndeleted.rue' ]; then
  pass "status paths: A/M/D projection"
else
  fail "status paths: A/M/D projection"
fi
TESTS=$((TESTS + 1))
if printf 'R100\told.rue\tnew.rue\n' | "${AFFECTED[@]}" status-paths >/dev/null 2>&1; then
  fail "status paths: unknown status unexpectedly accepted"
else
  pass "status paths: unknown status fails open"
fi

# --- selective integration: Git status + BTD + Buck invocation -------------

TESTS=$((TESTS + 1))
integration_root="$(mktemp -d)"
integration_cleanup() { rm -rf "$integration_root"; }
trap integration_cleanup EXIT
mkdir -p "$integration_root/scripts" "$integration_root/bin" "$integration_root/docs"
cp "$SCRIPTS_DIR/affected-targets" "$SCRIPTS_DIR/parse-btd-impacted.py" "$integration_root/scripts/"
chmod +x "$integration_root/scripts/affected-targets"

cat >"$integration_root/bin/fake-buck" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then output="$2"; shift 2; continue; fi
  shift
done
[ -n "$output" ]
printf '{"target":"root//:spec-tests"}\n' >"$output"
EOF
cat >"$integration_root/bin/fake-btd" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"${RUE_AFFECTED_BTD_ARGS:?}"
changes=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--changes" ]; then changes="$2"; shift 2; continue; fi
  shift
done
cmp -s "$changes" "${RUE_AFFECTED_EXPECTED_CHANGES:?}"
printf '{"target":"root//:spec-tests"}\n'
EOF
chmod +x "$integration_root/bin/fake-buck" "$integration_root/bin/fake-btd"
git -C "$integration_root" init -q
git -C "$integration_root" config user.email tests@example.invalid
git -C "$integration_root" config user.name affected-targets-test
printf 'before\n' >"$integration_root/docs/input.txt"
git -C "$integration_root" add . && git -C "$integration_root" commit -qm base
printf 'after\n' >"$integration_root/docs/input.txt"
git -C "$integration_root" add docs/input.txt && git -C "$integration_root" commit -qm head
printf 'M\tdocs/input.txt\n' >"$integration_root/expected-changes"
if (
  cd "$integration_root" &&
  RUE_AFFECTED_BASE_SHA=HEAD~1 \
  RUE_AFFECTED_HEAD_SHA=HEAD \
  RUE_AFFECTED_BTD="$integration_root/bin/fake-btd" \
  RUE_AFFECTED_BUCK2="$integration_root/bin/fake-buck" \
  RUE_AFFECTED_BTD_ARGS="$integration_root/btd-args" \
  RUE_AFFECTED_EXPECTED_CHANGES="$integration_root/expected-changes" \
  GITHUB_OUTPUT="$integration_root/output" \
  scripts/affected-targets decide >/dev/null
) && grep -Fxq 'full=false' "$integration_root/output" && \
    grep -Fxq 'selected=//:spec-tests' "$integration_root/output" && \
    grep -Fxq -- '--vcs' "$integration_root/btd-args" && \
    grep -Fxq -- 'git' "$integration_root/btd-args" && \
    grep -Fxq -- '--buck' "$integration_root/btd-args" && \
    grep -Fxq -- "$integration_root/bin/fake-buck" "$integration_root/btd-args"; then
  pass "integration: BTD selects a corpus from Git status and receives pinned Buck wrapper"
else
  fail "integration: selective BTD decision contract"
fi

# ---------------------------------------------------------------------------

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "=== affected-targets tests: PASSED ($TESTS checks) ==="
  exit 0
else
  echo "=== affected-targets tests: FAILED ($FAILURES/$TESTS checks) ===" >&2
  exit 1
fi
