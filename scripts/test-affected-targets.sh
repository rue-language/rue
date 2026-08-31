#!/usr/bin/env bash
# test-affected-targets.sh — regression tests for the deterministic logic of
# the affected-corpus selection (RUE-1119): the out-of-graph force-full matcher
# in scripts/affected-targets and the fail-open gate in scripts/ci-corpus-selected.
#
# These cover the parts that decide COVERAGE and must never regress: a path that
# must force a full run, a path that legitimately may be selected out, strict
# BTD decoding, the gate's fail-open behavior, the build scope's corpus-action
# deferral, and one selective end-to-end decision with local BTD/Buck stubs. The
# suite needs neither network nor Buck: everything that would consult the graph
# goes through `RUE_AFFECTED_BUCK2`. That is not only hygiene — this suite runs
# as an sh_test under buck2, and a real nested query is refused with rc=3.
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
REPO_ROOT="$(cd "$SCRIPTS_DIR/.." && pwd)"
DECISION=(bash "$SCRIPTS_DIR/ci-corpus-decision")
PARSER=(python3 "$SCRIPTS_DIR/parse-btd-impacted.py")
PAYLOAD=(python3 "$SCRIPTS_DIR/ci-affected-payload.py")
CLIPPY=(bash "$SCRIPTS_DIR/ci-clippy")

# Native lane membership is intentionally graph-derived. Keep these shell
# tests hermetic by supplying a tiny live-graph stand-in for the representative
# target checks; the integration case below provides its own Buck stand-in.
native_graph_stub="$(mktemp)"
cat >"$native_graph_stub" <<'EOF'
#!/usr/bin/env bash
case "${2:-}" in
  kind*)
    printf '%s\n' \
      root//crates/rue-codegen:rue-codegen-clippy \
      root//crates/rue-codegen:rue-codegen-debug-assert-check ;;
  *rue_ci_clippy_lane*)
    printf '%s\n' root//crates/rue-codegen:rue-codegen-clippy ;;
  *) printf '%s\n' root//crates/rue-codegen:rue-codegen-test ;;
esac
EOF
chmod +x "$native_graph_stub"
export RUE_AFFECTED_BUCK2="$native_graph_stub"

integration_root=""
test_cleanup() {
  rm -f "$native_graph_stub"
  if [ -n "$integration_root" ]; then
    rm -rf "$integration_root"
  fi
}
trap test_cleanup EXIT

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
expect_full "scripts/ci-affected-payload.py"
expect_full "scripts/ci-clippy"
expect_full "scripts/affected-targets"
expect_full "btd"
expect_full "scripts/provision-build-cache"
expect_full "scripts/install-valgrind"
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

# --- RUE-1130: named lanes use the same gate and the same safety model -------

lane_gate() { # lane_gate <full> <selected-lanes> <lane>; echoes exit status
  RUE_AFFECTED_FULL="$1" RUE_AFFECTED_LANES="$2" "${GATE[@]}" "$3" >/dev/null 2>&1
  echo $?
}

check_lane() { # check_lane <desc> <expected-status> <full> <lanes> <lane>
  local desc="$1" expected="$2" got
  TESTS=$((TESTS + 1))
  got="$(lane_gate "$3" "$4" "$5")"
  if [ "$got" = "$expected" ]; then
    pass "lane: $desc"
  else
    fail "lane: $desc (expected exit $expected, got $got)"
  fi
}

# Every gated lane must be recognized; an unrecognized name would silently fall
# through to the corpus path and be treated as an unknown target (fail-open),
# which is safe but would make the lane permanently ungated.
for lane in clippy native-linux-arm64 native-macos-arm64 release valgrind asan compiler-reproducibility rue-program-digests; do
  TESTS=$((TESTS + 1))
  if "${AFFECTED[@]}" is-selectable-lane "$lane"; then
    pass "lane: $lane is selectable"
  else
    fail "lane: $lane is not selectable (would never be gated)"
  fi
done

TESTS=$((TESTS + 1))
if [ "$("${AFFECTED[@]}" clippy-owned-targets)" = \
    "//crates/rue-codegen:rue-codegen-clippy" ]; then
  pass "clippy: owner-label query matches the canonical test in the stub graph"
else
  fail "clippy: owner-label query drifted from the canonical test in the stub graph"
fi

TESTS=$((TESTS + 1))
usage_status=0
usage_output="$("${AFFECTED[@]}" not-a-command 2>&1)" || usage_status=$?
if [ "$usage_status" -eq 2 ] && grep -Fq 'clippy-owned-targets' <<<"$usage_output"; then
  pass "affected-targets: catch-all usage lists clippy-owned-targets"
else
  fail "affected-targets: catch-all usage omits clippy-owned-targets"
fi

# Each lane must name at least one representative Buck target, or it could never
# be selected and would be deselected on every selective run.
for lane in clippy native-linux-arm64 native-macos-arm64 release valgrind asan compiler-reproducibility rue-program-digests; do
  TESTS=$((TESTS + 1))
  if [ -n "$("${AFFECTED[@]}" lane-targets "$lane")" ]; then
    pass "lane: $lane declares representative targets"
  else
    fail "lane: $lane declares no targets (would always deselect)"
  fi
done

check_lane "full=true runs lane" 0 "true" "" "valgrind"
check_lane "selected lane runs" 0 "false" "valgrind asan" "valgrind"
check_lane "unselected lane deselected" 1 "false" "asan" "valgrind"
check_lane "empty lane selection deselects" 1 "false" "" "valgrind"
check_lane "unset full runs lane (fail-open)" 0 "" "" "valgrind"
check_lane "malformed lane selection is an error" 2 "false" "not-a-lane" "valgrind"
check_lane "selected clippy lane runs" 0 "false" "clippy" "clippy"
check_lane "unselected clippy lane is intentionally deselected" 1 "false" "release" "clippy"
# A lane name must not be satisfied by a corpus target list, and vice versa:
# the two selections are read from different environment variables.
check_lane "corpus selection does not select a lane" 1 "false" "" "release"
TESTS=$((TESTS + 1))
if [ "$(RUE_AFFECTED_FULL=false RUE_AFFECTED_TARGETS="//:spec-tests" RUE_AFFECTED_LANES="" "${GATE[@]}" "//:spec-tests" >/dev/null 2>&1; echo $?)" = "0" ]; then
  pass "lane: corpus targets still gate on RUE_AFFECTED_TARGETS"
else
  fail "lane: corpus gating regressed"
fi

# RUE-1265: the duplication gate reads this script's corpus and lane
# inventories instead of keeping a second copy of either. `lanes` must stay
# complete — a lane missing from it is a lane the duplication gate never learns
# to classify — and `corpus-targets` must stay non-empty, since an empty
# platform-corpus inventory would silently remove that lane from the gate's
# view of what runs.
TESTS=$((TESTS + 1))
if [ "$("${AFFECTED[@]}" lanes | sort | tr '\n' ' ')" \
    = "asan clippy compiler-reproducibility native-linux-arm64 native-macos-arm64 release rue-program-digests valgrind " ]; then
  pass "lanes: the gated lane inventory is exposed in full"
else
  fail "lanes: printed $("${AFFECTED[@]}" lanes | tr '\n' ' ')"
fi

TESTS=$((TESTS + 1))
# Capture first: `grep -q` behind a pipe under `set -o pipefail` kills the
# producer with EPIPE on its first match (RUE-1011/RUE-1155).
corpus_inventory="$("${AFFECTED[@]}" corpus-targets)"
if grep -q '^//:spec-tests$' <<<"$corpus_inventory"; then
  pass "corpus-targets: the platform-corpus inventory is exposed"
else
  fail "corpus-targets: //:spec-tests is missing from the exposed inventory"
fi

# The asan harness is outside the Buck graph, so BTD cannot see it; its sources
# must force a full run rather than relying on a representative target.
expect_full "crates/rue-runtime-asan/src/main.rs"
expect_full "crates/rue-runtime-asan/Cargo.toml"

# --- RUE-1130: narrowing a lane to the impacted closure ---------------------

# The registry is the one source of truth for every narrowed-lane scope.
TESTS=$((TESTS + 1))
if [ "$(${AFFECTED[@]} scope-registry | tr '\n' ' ')" = \
     "clippy|graph|crates_sh_test_ending_-clippy linux-premerge-build|pattern|//crates/... linux-premerge-tests|graph|rue_test_tier_premerge-minus-dedicated-and-clippy native-platforms-units|graph|rue_platform_native " ]; then
  pass "narrow: scope registry declares every consumer"
else
  fail "narrow: scope registry drifted"
fi

# The planner and every narrowing consumer read the same canonical threshold.
TESTS=$((TESTS + 1))
if [ "$("${AFFECTED[@]}" narrow-limit)" = "600" ]; then
  pass "narrow: default canonical limit is exposed"
else
  fail "narrow: default canonical limit is unavailable"
fi
TESTS=$((TESTS + 1))
if [ "$(RUE_AFFECTED_NARROW_LIMIT=7 "${AFFECTED[@]}" narrow-limit)" = "7" ]; then
  pass "narrow: supported custom limit has one authority"
else
  fail "narrow: custom canonical limit was ignored"
fi
TESTS=$((TESTS + 1))
if RUE_AFFECTED_NARROW_LIMIT=00 "${AFFECTED[@]}" narrow-limit >/dev/null 2>&1; then
  fail "narrow: malformed canonical limit was accepted"
else
  pass "narrow: malformed canonical limit fails open"
fi
TESTS=$((TESTS + 1))
if ! "${AFFECTED[@]}" scope-targets future-lane >/dev/null 2>&1; then
  pass "narrow: an unregistered future lane is rejected"
else
  fail "narrow: an unregistered future lane was accepted"
fi

# `intersect` is what turns a lane's fixed target list into the impacted subset.
# Order follows the lane's own list so the runner's output stays stable.
narrow_root="$(mktemp -d)"
printf '//crates/rue-codegen:rue-codegen-test\n//crates/rue-target:rue-target-test\n' >"$narrow_root/impacted"
TESTS=$((TESTS + 1))
if [ "$("${AFFECTED[@]}" intersect "$narrow_root/impacted" \
        //crates/rue-compiler:rue-compiler-test \
        //crates/rue-target:rue-target-test \
        //crates/rue-codegen:rue-codegen-test | tr '\n' ' ')" \
     = "//crates/rue-target:rue-target-test //crates/rue-codegen:rue-codegen-test " ]; then
  pass "narrow: intersect keeps the lane's order and drops unimpacted targets"
else
  fail "narrow: intersect produced the wrong subset"
fi

# An empty impacted file must yield an empty intersection rather than the whole
# list; the caller distinguishes the two by the determinator's `narrowed` flag,
# never by this output.
: >"$narrow_root/none"
TESTS=$((TESTS + 1))
if [ -z "$("${AFFECTED[@]}" intersect "$narrow_root/none" //crates/rue-target:rue-target-test)" ]; then
  pass "narrow: empty impacted list intersects to nothing"
else
  fail "narrow: empty impacted list did not intersect to nothing"
fi

# The graph lane is narrowed by intersecting its registered live allowlist.
printf '%s\n' //crates/rue-codegen:rue-codegen-test //crates/rue-other:other-test >"$narrow_root/native-impacted"
TESTS=$((TESTS + 1))
native_subset="$(RUE_AFFECTED_BUCK2="$native_graph_stub" "${AFFECTED[@]}" narrow-scope native-platforms-units "$narrow_root/native-impacted")"
if [ "$native_subset" = "//crates/rue-codegen:rue-codegen-test" ]; then
  pass "narrow: native output is an intersection of its registered scope"
else
  fail "narrow: native output escaped its registered scope"
fi
TESTS=$((TESTS + 1))
if ! RUE_AFFECTED_BUCK2="$narrow_root/absent" "${AFFECTED[@]}" narrow-scope native-platforms-units "$narrow_root/native-impacted" >/dev/null 2>&1; then
  pass "narrow: unavailable native graph is not reported as verified"
else
  fail "narrow: unavailable native graph unexpectedly verified a subset"
fi

cat >"$narrow_root/native-two-buck" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' \
  root//crates/rue-codegen:rue-codegen-test \
  root//crates/rue-target:rue-target-test
EOF
chmod +x "$narrow_root/native-two-buck"
scope_summary="$narrow_root/native-scope-summary"
TESTS=$((TESTS + 1))
if GITHUB_STEP_SUMMARY="$scope_summary" RUE_AFFECTED_BUCK2="$narrow_root/native-two-buck" \
    "${AFFECTED[@]}" narrow-scope native-platforms-units "$narrow_root/native-impacted" >/dev/null && \
    grep -Fq '**VERIFIED** subset; selected **1/2** targets; unweighted saved share **50.00%**' "$scope_summary"; then
  pass "narrow: verified scope summary reports exact selected and saved shares"
else
  fail "narrow: verified scope summary omitted exact saved-share math"
fi

degraded_scope_summary="$narrow_root/degraded-scope-summary"
TESTS=$((TESTS + 1))
if ! GITHUB_STEP_SUMMARY="$degraded_scope_summary" RUE_AFFECTED_BUCK2="$narrow_root/absent" \
    "${AFFECTED[@]}" narrow-scope native-platforms-units "$narrow_root/native-impacted" >/dev/null 2>&1 && \
    grep -Fq '**DEGRADED**; saved share **not applicable**' "$degraded_scope_summary" && \
    ! grep -Fq '**VERIFIED**' "$degraded_scope_summary"; then
  pass "narrow: degraded scope summary cannot masquerade as verified"
else
  fail "narrow: degraded scope summary is missing or contradictory"
fi

# RUE-1855: lane selection and narrowing must consume the same complete live
# set of crate-local sh_tests whose labels end exactly in `-clippy`.
cat >"$narrow_root/clippy-buck" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" != "uquery" ] || [ "${2:-}" != "kind('sh_test', 'root//crates/...')" ]; then
  echo "clippy-buck: unexpected query" >&2
  exit 1
fi
printf '%s\n' \
  root//crates/rue-alpha:rue-alpha-clippy \
  root//crates/rue-alpha:rue-alpha-debug-assert-check \
  root//crates/rue-beta:rue-beta-clippy
EOF
chmod +x "$narrow_root/clippy-buck"

TESTS=$((TESTS + 1))
clippy_lane="$(RUE_AFFECTED_BUCK2="$narrow_root/clippy-buck" "${AFFECTED[@]}" lane-targets clippy)"
clippy_scope="$(RUE_AFFECTED_BUCK2="$narrow_root/clippy-buck" "${AFFECTED[@]}" scope-targets clippy)"
if [ "$clippy_lane" = "$clippy_scope" ] && \
    [ "$(tr '\n' ' ' <<<"$clippy_scope")" = \
      "//crates/rue-alpha:rue-alpha-clippy //crates/rue-beta:rue-beta-clippy " ]; then
  pass "clippy: lane proxy and runnable scope share the exact live inventory"
else
  fail "clippy: lane proxy and runnable scope drifted"
fi

printf '%s\n' \
  //crates/rue-beta:rue-beta-clippy \
  //crates/removed:removed-clippy \
  //crates/rue-alpha:rue-alpha-debug-assert-check \
  >"$narrow_root/clippy-impacted"
TESTS=$((TESTS + 1))
clippy_subset="$(RUE_AFFECTED_BUCK2="$narrow_root/clippy-buck" "${AFFECTED[@]}" \
  narrow-scope clippy "$narrow_root/clippy-impacted")"
if [ "$clippy_subset" = "//crates/rue-beta:rue-beta-clippy" ]; then
  pass "clippy: narrowed scope keeps only live impacted -clippy targets"
else
  fail "clippy: narrowed scope retained a non-clippy or dead target"
fi

# A well-formed target removed from the live graph is safely absent, and an
# impacted closure with no clippy intersection is a verified empty subset.
printf '%s\n' //crates/removed:removed-clippy >"$narrow_root/clippy-stale-only"
clippy_empty_summary="$narrow_root/clippy-empty-summary"
TESTS=$((TESTS + 1))
if [ -z "$(GITHUB_STEP_SUMMARY="$clippy_empty_summary" \
    RUE_AFFECTED_BUCK2="$narrow_root/clippy-buck" "${AFFECTED[@]}" \
    narrow-scope clippy "$narrow_root/clippy-stale-only")" ] && \
    grep -Fq '**VERIFIED** subset; selected **0/2** targets; unweighted saved share **100.00%**' "$clippy_empty_summary"; then
  pass "clippy: stale-only impact is a visible verified empty subset"
else
  fail "clippy: stale-only impact did not produce the verified no-op contract"
fi

# Empty or malformed planner output is corrupt state, not a verified no-op.
TESTS=$((TESTS + 1))
if ! RUE_AFFECTED_BUCK2="$narrow_root/clippy-buck" "${AFFECTED[@]}" \
    narrow-scope clippy "$narrow_root/none" >/dev/null 2>&1; then
  pass "clippy: empty impacted output degrades to the full scope"
else
  fail "clippy: empty impacted output masqueraded as a verified subset"
fi
printf '%s\n' not-a-target >"$narrow_root/clippy-malformed"
TESTS=$((TESTS + 1))
if ! RUE_AFFECTED_BUCK2="$narrow_root/clippy-buck" "${AFFECTED[@]}" \
    narrow-scope clippy "$narrow_root/clippy-malformed" >/dev/null 2>&1; then
  pass "clippy: malformed impacted output degrades to the full scope"
else
  fail "clippy: malformed impacted output masqueraded as a verified subset"
fi

cat >"$narrow_root/clippy-empty-buck" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' root//crates/rue-alpha:rue-alpha-debug-assert-check
EOF
chmod +x "$narrow_root/clippy-empty-buck"
TESTS=$((TESTS + 1))
clippy_hard_error_summary="$narrow_root/clippy-hard-error-summary"
if GITHUB_STEP_SUMMARY="$clippy_hard_error_summary" \
    RUE_AFFECTED_BUCK2="$narrow_root/clippy-empty-buck" "${AFFECTED[@]}" \
    scope-targets clippy >/dev/null 2>&1; then
  fail "clippy: empty successful live query was accepted"
else
  clippy_status=$?
  if [ "$clippy_status" -eq 2 ]; then
    pass "clippy: empty successful live query remains a distinct hard error"
  else
    fail "clippy: empty successful live query returned $clippy_status instead of 2"
  fi
fi

TESTS=$((TESTS + 1))
if GITHUB_STEP_SUMMARY="$clippy_hard_error_summary" \
    RUE_AFFECTED_BUCK2="$narrow_root/clippy-empty-buck" "${AFFECTED[@]}" \
    narrow-scope clippy "$narrow_root/clippy-impacted" >/dev/null 2>&1; then
  fail "clippy: empty live inventory was accepted while narrowing"
else
  clippy_status=$?
  if [ "$clippy_status" -eq 2 ] && \
      grep -Fq '**FAILED**' "$clippy_hard_error_summary" && \
      grep -Fq 'live scope inventory rejected; hard failure' "$clippy_hard_error_summary" && \
      ! grep -Fq 'full scope used' "$clippy_hard_error_summary"; then
    pass "clippy: empty live inventory reports its hard failure accurately"
  else
    fail "clippy: empty live inventory summary misreported a full-scope fallback"
  fi
fi

TESTS=$((TESTS + 1))
if RUE_AFFECTED_BUCK2="$narrow_root/absent" "${AFFECTED[@]}" \
    scope-targets clippy >/dev/null 2>&1; then
  fail "clippy: failed live query was accepted"
else
  clippy_status=$?
  if [ "$clippy_status" -eq 1 ]; then
    pass "clippy: failed live query remains a fail-open scope error"
  else
    fail "clippy: failed live query returned unexpected status $clippy_status"
  fi
fi

# A missing file is an unavailable intersection, never a verified empty one.
TESTS=$((TESTS + 1))
if ! "${AFFECTED[@]}" intersect "$narrow_root/absent" //crates/rue-target:rue-target-test >/dev/null 2>&1; then
  pass "narrow: missing impacted file rejects the intersection"
else
  fail "narrow: missing impacted file masqueraded as an empty intersection"
fi

# grep rc=1 is an ordinary non-member, but rc>1 means the intersection was not
# computed. Pin that distinction without depending on a real filesystem fault.
mkdir -p "$narrow_root/failing-grep-bin"
cat >"$narrow_root/failing-grep-bin/grep" <<'EOF'
#!/usr/bin/env bash
exit 2
EOF
chmod +x "$narrow_root/failing-grep-bin/grep"
TESTS=$((TESTS + 1))
if ! PATH="$narrow_root/failing-grep-bin:$PATH" "${AFFECTED[@]}" intersect \
    "$narrow_root/native-impacted" //crates/rue-codegen:rue-codegen-test >/dev/null 2>&1; then
  pass "narrow: an intersection read error is not reported as verified empty"
else
  fail "narrow: an intersection read error was treated as an ordinary non-member"
fi

scope_root="$(mktemp -d)"
mkdir -p "$scope_root/bin"
cat >"$scope_root/bin/fake-buck" <<'FAKEBUCK'
#!/usr/bin/env bash
# Answers the ownership and closure queries build_scope() makes, and nothing
# else. The cquery result is a reverse-dependency closure, not a name-derived
# action/test pair: `corpus-carrier` intentionally has no matching suffix.
contains_target() {
  local query="$1" target="$2" tokenized
  tokenized="${query//set(/ }"
  tokenized="${tokenized//)/ }"
  case " $tokenized " in
    *" $target "*) return 0 ;;
    *) return 1 ;;
  esac
}
case "$1" in
  uquery)
    case "$2" in
      set\(//...\ toolchains//...\))
        printf '%s\n' \
          root//:cli-tests-action \
          root//:spec-tests-action \
          root//crates/rue-compiler:rue-compiler \
          root//crates/rue-span:rue-span-test ;;
      attrfilter*)
        # The premerge-tier selection. Deliberately omits `newthing-test`, so
        # that action is owned by no lane.
        printf '%s\n' \
          root//crates/rue-oracle-diff:oracle-diff-test \
          root//crates/rue-oracle-diff:oracle-diff-spec-test ;;
      //crates/...*)
        if [[ "$2" == *' except set('* ]] \
            && contains_target "$2" '//crates/rue-oracle-diff:oracle-diff-test-action' \
            && contains_target "$2" '//crates/rue-oracle-diff:oracle-diff-test' \
            && contains_target "$2" '//crates/rue-oracle-diff:corpus-carrier' \
            && contains_target "$2" '//crates/rue-oracle-diff:oracle-diff-spec-test-action' \
            && contains_target "$2" '//crates/rue-oracle-diff:oracle-diff-spec-test'; then
          # The production query must subtract every reverse-dependent target
          # returned by cquery. If it drops the except/set expression, this
          # branch deliberately returns the pre-fix full crate scope instead.
          printf '%s\n' \
            root//crates/rue-compiler:rue-compiler \
            root//crates/rue-newthing:newthing-test-action \
            root//crates/rue-span:rue-span-test
        else
          printf '%s\n' \
            root//crates/rue-compiler:rue-compiler \
            root//crates/rue-oracle-diff:oracle-diff-test-action \
            root//crates/rue-oracle-diff:oracle-diff-test \
            root//crates/rue-oracle-diff:corpus-carrier \
            root//crates/rue-newthing:newthing-test-action \
            root//crates/rue-span:rue-span-test
        fi
        ;;
      *) echo "fake-buck: unexpected uquery: $2" >&2; exit 1 ;;
    esac
    ;;
  cquery)
    case "$2" in
      deps\(set\(*\))
        # Model the graph when the test asks whether a produced scope still
        # reaches a deferred action. A scope containing the carrier or wrapper
        # would expose the action here; a clean scope returns no deferred
        # action at all.
        if [[ "$2" == *'rue-oracle-diff:oracle-diff-test'* \
            || "$2" == *'rue-oracle-diff:corpus-carrier'* ]]; then
          printf '%s\n' root//crates/rue-oracle-diff:oracle-diff-test-action
        fi
        if [[ "$2" == *'rue-oracle-diff:oracle-diff-spec-test'* ]]; then
          printf '%s\n' root//crates/rue-oracle-diff:oracle-diff-spec-test-action
        fi
        ;;
      rdeps\(//crates/...,\ kind\(\'_corpus_action\',\ deps\(set\(*\)\)\)\))
        # Include configured labels to pin normalization and a transitive
        # carrier whose spelling is unrelated to its action.
        printf '%s\n' \
          'root//crates/rue-oracle-diff:oracle-diff-test-action (linux)' \
          'root//crates/rue-oracle-diff:oracle-diff-test (linux)' \
          'root//crates/rue-oracle-diff:corpus-carrier (linux)' \
          'root//crates/rue-oracle-diff:oracle-diff-spec-test-action (linux)' \
          'root//crates/rue-oracle-diff:oracle-diff-spec-test (linux)' ;;
      *) echo "fake-buck: unexpected cquery: $2" >&2; exit 1 ;;
    esac
    ;;
  *) echo "fake-buck: unexpected command: $1" >&2; exit 1 ;;
esac
FAKEBUCK
chmod +x "$scope_root/bin/fake-buck"
SCOPED=("${AFFECTED[@]}")

assert_scope_closure_clear() { # assert_scope_closure_clear <description> <scope>
  local desc="$1" scope="$2" labels closure
  labels="${scope//$'\n'/ }"
  closure="$("$scope_root/bin/fake-buck" cquery "deps(set($labels))")"
  TESTS=$((TESTS + 1))
  if grep -Fq -- '//crates/rue-oracle-diff:oracle-diff-test-action' <<<"$closure" \
      || grep -Fq -- '//crates/rue-oracle-diff:oracle-diff-spec-test-action' <<<"$closure"; then
    fail "narrow: $desc still reaches a deferred corpus action"
  else
    pass "narrow: $desc dependency closure has no deferred corpus action"
  fi
}

# `build-scope` is what keeps a pattern lane's narrowing a SUBSET of what the
# lane built before: exactly `//crates/...`, the scope linux-premerge built
# before RUE-1130 narrowed it. The root-level entries here are
# `cached_corpus_suite` actions that `//crates/...` never matched, and letting
# one through re-runs a whole corpus inside linux-premerge — `buck2 build` runs
# the action, and `rue_ci_dedicated_lane` is a `buck2 test` label filter that
# cannot reach it.
#
# `//crates/rue-oracle-diff:oracle-diff-test-action` is now DROPPED too. It was
# kept when this test was written, on the reasoning that `//crates/...` had
# always matched it so removing it would be a behavior change rather than a
# restoration. It is the behavior change RUE-1511 asks for: that action and its
# spec sibling ran 140.9s and 87.1s inside `Build all targets` — 78.9% of a step
# whose median is 304s — concurrently with the dedicated
# `test (linux-x64-oracle-diff*)` lanes that exist to own them. The exclusion no
# longer needs labels on the action, which is what "tracked separately" was
# waiting for: it derives owned `_corpus_action` nodes from
# `deps(required selection)` and excludes the same `rdeps(//crates/..., owned)`
# closure from either build-scope spelling.
printf '%s\n' \
  //crates/rue-compiler:rue-compiler \
  //:cli-tests \
  //:cli-tests-action \
  //crates/rue-codegen:rue-codegen-test \
  //:spec-tests-action \
  //:cli-tests-shard-2-action \
  //crates/rue-oracle-diff:oracle-diff-test-action \
  //crates/rue-oracle-diff:corpus-carrier \
  >"$narrow_root/mixed"
TESTS=$((TESTS + 1))
narrowed_scope="$(RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" "${AFFECTED[@]}" build-scope "$narrow_root/mixed")"
if [ "$(tr '\n' ' ' <<<"$narrowed_scope")" \
     = "//crates/rue-compiler:rue-compiler " ]; then
  pass "narrow: build-scope is the exact impacted/live-scope intersection"
else
  fail "narrow: build-scope did not restore the //crates/... scope"
fi
assert_scope_closure_clear "narrowed build-scope" "$narrowed_scope"

# RUE-1788: `build-scope` applies one configured live-graph reverse-dependency
# closure to both narrowed and unnarrowed scopes, so these cases stub Buck
# exactly as the end-to-end decision case below does.
# A real `./buck2` here would be a RECURSIVE Buck invocation — this suite runs
# as an sh_test under buck2, which refuses the nested query with rc=3 — and the
# suite's contract is that it needs neither network nor Buck.

# The unnarrowed spelling loses independently of the narrowed one, and it is the
# common case — a compiler change impacts more than NARROW_LIMIT targets, so it
# is never narrowed. Both must apply the same reverse-dependency exclusion, or
# fixing one leaves premerge building the corpora exactly as before.
TESTS=$((TESTS + 1))
unnarrowed="$(RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" "${AFFECTED[@]}" build-scope 2>/dev/null)"
if [ -n "$unnarrowed" ] \
    && grep -Fxq -- '//crates/rue-newthing:newthing-test-action' <<<"$unnarrowed" \
    && ! grep -Fq -- '//crates/rue-oracle-diff:oracle-diff-test-action' <<<"$unnarrowed" \
    && ! grep -Fq -- '//crates/rue-oracle-diff:corpus-carrier' <<<"$unnarrowed"; then
  pass "narrow: build-scope with no list excludes the deferred dependency closure"
else
  fail "narrow: build-scope with no list kept a deferred closure or dropped an unowned action"
fi
assert_scope_closure_clear "unnarrowed build-scope" "$unnarrowed"

# FAIL CLOSED is the property the whole change rests on: an action no required
# lane runs must stay in the build, because a corpus nothing executes is the
# RUE-924 false green. `newthing-test-action` is owned by no lane in the stub,
# so it must survive both spellings.
printf '%s\n' \
  //crates/rue-oracle-diff:oracle-diff-test-action \
  //crates/rue-newthing:newthing-test-action \
  //crates/rue-span:rue-span-test >"$narrow_root/unowned"
TESTS=$((TESTS + 1))
kept_unowned="$(RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" "${AFFECTED[@]}" build-scope "$narrow_root/unowned" 2>/dev/null | tr '\n' ' ')"
if [ "$kept_unowned" = "//crates/rue-newthing:newthing-test-action //crates/rue-span:rue-span-test " ]; then
  pass "narrow: build-scope keeps a corpus action no required lane owns"
else
  fail "narrow: build-scope deferred an action nothing runs (got '$kept_unowned')"
fi

# When Buck cannot answer, the caller must build exactly what it built before:
# the narrowed list unfiltered, so a query outage costs wall time and never
# coverage.
TESTS=$((TESTS + 1))
degraded="$(RUE_AFFECTED_BUCK2="$scope_root/bin/absent" "${AFFECTED[@]}" build-scope "$narrow_root/unowned" 2>/dev/null | tr '\n' ' ')"
if [ "$degraded" = "//crates/... " ]; then
  pass "narrow: an unanswerable Buck query falls open to the full scope"
else
  fail "narrow: a failed query did not fall open (got '$degraded')"
fi

# A successful ownership query followed by a failed configured closure query
# must still fail open. Exercise both scope spellings: the narrowed request
# falls back to the complete pattern, just like the unnarrowed request.
cat >"$scope_root/bin/closure-fail-buck" <<'FAILING_CLOSURE'
#!/usr/bin/env bash
case "$1" in
  uquery)
    case "$2" in
      attrfilter*)
        printf '%s\n' \
          root//crates/rue-oracle-diff:oracle-diff-test \
          root//crates/rue-oracle-diff:oracle-diff-spec-test ;;
      //crates/...*)
        printf '%s\n' \
          root//crates/rue-compiler:rue-compiler \
          root//crates/rue-oracle-diff:oracle-diff-test-action \
          root//crates/rue-newthing:newthing-test-action \
          root//crates/rue-span:rue-span-test ;;
      *) exit 1 ;;
    esac
    ;;
  cquery)
    echo "closure-fail-buck: configured closure unavailable" >&2
    exit 1
    ;;
  *) exit 1 ;;
esac
FAILING_CLOSURE
chmod +x "$scope_root/bin/closure-fail-buck"
TESTS=$((TESTS + 1))
closure_failed_narrow="$(RUE_AFFECTED_BUCK2="$scope_root/bin/closure-fail-buck" \
  "${AFFECTED[@]}" build-scope "$narrow_root/unowned" 2>/dev/null | tr '\n' ' ')"
if [ "$closure_failed_narrow" = "//crates/... " ]; then
  pass "narrow: a failed configured closure falls open to the full scope"
else
  fail "narrow: a failed configured closure changed the narrowed scope (got '$closure_failed_narrow')"
fi
TESTS=$((TESTS + 1))
closure_failed_unnarrowed="$(RUE_AFFECTED_BUCK2="$scope_root/bin/closure-fail-buck" \
  "${AFFECTED[@]}" build-scope 2>/dev/null | tr '\n' ' ')"
if [ "$closure_failed_unnarrowed" = "//crates/... " ]; then
  pass "narrow: a failed configured closure falls open to the full unnarrowed scope"
else
  fail "narrow: a failed configured closure changed the unnarrowed scope (got '$closure_failed_unnarrowed')"
fi

# An impacted closure naming only corpora is legitimate — corpus data can change
# without reaching a crate — and must read as "nothing to build", never as the
# whole pattern. The workflow prints that case rather than silently skipping.
printf '%s\n' //:cli-tests-action //:spec-tests-action >"$narrow_root/corpora-only"
TESTS=$((TESTS + 1))
if [ -z "$(RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" "${AFFECTED[@]}" build-scope "$narrow_root/corpora-only")" ]; then
  pass "narrow: build-scope on a corpus-only closure yields nothing to build"
else
  fail "narrow: build-scope invented crate targets from a corpus-only closure"
fi

corpus_scope_summary="$narrow_root/corpus-scope-summary"
TESTS=$((TESTS + 1))
if [ -z "$(GITHUB_STEP_SUMMARY="$corpus_scope_summary" RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" \
    "${AFFECTED[@]}" narrow-scope linux-premerge-build "$narrow_root/corpora-only")" ] && \
    grep -Fq '**VERIFIED** subset; selected **0/3** targets; unweighted saved share **100.00%**' "$corpus_scope_summary"; then
  pass "narrow: corpus-only closure takes the registered empty build subset"
else
  fail "narrow: corpus-only registered build subset or summary is wrong"
fi

TESTS=$((TESTS + 1))
test_subset="$(RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" "${AFFECTED[@]}" \
  narrow-scope linux-premerge-tests "$narrow_root/corpora-only")"
if [ -z "$test_subset" ]; then
  pass "narrow: corpus-only closure takes the registered empty premerge test subset"
else
  fail "narrow: corpus-only premerge test scope invented a runnable test"
fi

strict_scope_summary="$narrow_root/strict-scope-summary"
TESTS=$((TESTS + 1))
if ! GITHUB_STEP_SUMMARY="$strict_scope_summary" RUE_AFFECTED_BUCK2="$scope_root/bin/absent" \
    "${AFFECTED[@]}" narrow-scope linux-premerge-build "$narrow_root/unowned" >/dev/null 2>&1 && \
    grep -Fq '`linux-premerge-build`: **DEGRADED**' "$strict_scope_summary" && \
    ! grep -Fq '**VERIFIED**' "$strict_scope_summary"; then
  pass "narrow: strict linux scope failure reports degraded full-scope fallback"
else
  fail "narrow: strict linux scope failure did not report an unverified fallback"
fi

# An unreadable list must fail loudly so the caller can fall open to the full
# pattern; silently yielding nothing would turn it into "build nothing".
TESTS=$((TESTS + 1))
if RUE_AFFECTED_BUCK2="$scope_root/bin/fake-buck" "${AFFECTED[@]}" build-scope "$narrow_root/absent" >/dev/null 2>&1; then
  fail "narrow: build-scope accepted a missing impacted file"
else
  pass "narrow: build-scope rejects a missing impacted file"
fi

# test.sh must fall back to the full pattern for every input that is not a
# readable, non-empty list, and must never turn a bad list into "run nothing".
run_test_sh_args() { # run_test_sh_args [VAR=VALUE ...] -> prints the buck2 test args
  local shim="$narrow_root/bin"
  mkdir -p "$shim"
  printf '#!/usr/bin/env bash\nshift\n[ "${1:-}" = test ] && { shift; printf "ARGS: %%s\\n" "$*"; exit 0; }\nexit 0\n' >"$shim/dotslash"
  chmod +x "$shim/dotslash"
  ( cd "$REPO_ROOT" && PATH="$shim:$PATH" CI=true RUE_TEST_TIER=premerge env "$@" ./test.sh 2>&1 ) | grep '^ARGS: ' || true
}

check_narrow() { # check_narrow <desc> <expect-substring> [VAR=VALUE ...]
  local desc="$1" expect="$2"; shift 2
  TESTS=$((TESTS + 1))
  # Capture first: `grep -q` exits on its first match, and behind a pipe under
  # `set -o pipefail` that kills the producer with EPIPE and fails the pipeline
  # (RUE-1011/RUE-1155). `--` so an expectation starting with a flag is not
  # parsed as one.
  local actual
  actual="$(run_test_sh_args "$@")"
  if grep -Fq -- "$expect" <<<"$actual"; then
    pass "narrow: $desc"
  else
    fail "narrow: $desc (expected args containing '$expect')"
  fi
}

printf '//crates/rue-span:rue-span-test\n' >"$narrow_root/one"
check_narrow "a readable list narrows the suite" \
  "ARGS: //crates/rue-span:rue-span-test --always-exclude" \
  "RUE_TEST_TARGETS_FILE=$narrow_root/one"
check_narrow "an empty list runs the full pattern" \
  "ARGS: //... toolchains//..." \
  "RUE_TEST_TARGETS_FILE=$narrow_root/none"
TESTS=$((TESTS + 1))
if [ -z "$(run_test_sh_args "RUE_TEST_TARGETS_FILE=$narrow_root/none" "RUE_TEST_TARGETS_STATUS=VERIFIED")" ]; then
  pass "narrow: a verified empty test scope runs no tests"
else
  fail "narrow: a verified empty test scope fell back to the full pattern"
fi
check_narrow "an unreadable list runs the full pattern" \
  "ARGS: //... toolchains//..." \
  "RUE_TEST_TARGETS_FILE=$narrow_root/absent"
check_narrow "an unset list runs the full pattern" \
  "ARGS: //... toolchains//..."
# Narrowing must not weaken the tier or deferral filters it runs under.
check_narrow "a narrowed suite keeps its tier and deferral filters" \
  "--include rue_test_tier_premerge --exclude rue_ci_dedicated_lane" \
  "RUE_TEST_TARGETS_FILE=$narrow_root/one"

# RUE-1506. test.sh read this list with `mapfile`, a Bash 4 builtin macOS does
# not have, so every narrowed run on a Mac exited 127 while this suite stayed
# green on Linux CI — the only place it runs. These cases pin the reading
# itself, in behavior a Bash 5 host can also see: each expectation below is
# wrong under `mapfile -t`, or under a read loop missing one of its guards, so
# a reintroduction fails here rather than only on a developer's machine.

# A blank line is not a target. `mapfile` keeps it and buck2 receives an empty
# argument between the two real ones.
printf '//crates/rue-span:rue-span-test\n\n//crates/rue-parser:rue-parser-test\n' >"$narrow_root/blank-line"
check_narrow "blank lines in the list are not passed to buck2" \
  "ARGS: //crates/rue-span:rue-span-test //crates/rue-parser:rue-parser-test --always-exclude" \
  "RUE_TEST_TARGETS_FILE=$narrow_root/blank-line"

# The producer's `sed '/^$/d'` drops empty lines but not whitespace-only ones,
# so this is the reachable half: it must fall open, not narrow to " ".
printf '   \n\t\n' >"$narrow_root/whitespace"
check_narrow "a whitespace-only list runs the full pattern" \
  "ARGS: //... toolchains//..." \
  "RUE_TEST_TARGETS_FILE=$narrow_root/whitespace"
check_narrow "a VERIFIED whitespace-only list still runs the full pattern" \
  "ARGS: //... toolchains//..." \
  "RUE_TEST_TARGETS_FILE=$narrow_root/whitespace" \
  "RUE_TEST_TARGETS_STATUS=VERIFIED"

# A file whose last line has no newline. `mapfile` keeps that line; a plain
# `while read` loop silently drops it, dropping a target from the run.
printf '//crates/rue-span:rue-span-test' >"$narrow_root/unterminated"
check_narrow "a final line without a newline is still a target" \
  "ARGS: //crates/rue-span:rue-span-test --always-exclude" \
  "RUE_TEST_TARGETS_FILE=$narrow_root/unterminated"

# A directory passes -r and -s, and `read` fails on it without assigning, which
# left the loop's `-n` fallback expanding an unset variable: `set -u` killed the
# whole suite over an input the fail-open contract says must fall back.
mkdir -p "$narrow_root/a-directory"
check_narrow "a directory runs the full pattern instead of failing the suite" \
  "ARGS: //... toolchains//..." \
  "RUE_TEST_TARGETS_FILE=$narrow_root/a-directory"

# The workflow-facing adapter must reserve `run=false` for the gate's explicit
# deselection status. A gate crash or missing executable runs the corpus.
decision_root="$(mktemp -d)"
printf '#!/usr/bin/env bash\nexit 1\n' >"$decision_root/deselect"
printf '#!/usr/bin/env bash\nexit 2\n' >"$decision_root/crash"
chmod +x "$decision_root/deselect" "$decision_root/crash"
check_decision() { # check_decision <description> <gate> <expected-output> <expected-status>
  local desc="$1" gate="$2" expected="$3" expected_status="$4"
  local output="$decision_root/output"
  TESTS=$((TESTS + 1))
  : >"$output"
  if RUE_AFFECTED_GATE="$gate" GITHUB_OUTPUT="$output" "${DECISION[@]}" "//:spec-tests" >/dev/null 2>&1 && \
      grep -Fxq "run=$expected" "$output" && \
      grep -Fxq "gate_status=$expected_status" "$output"; then
    pass "decision: $desc"
  else
    fail "decision: $desc"
  fi
}
check_decision "exit 1 intentionally deselects" "$decision_root/deselect" false DESELECTED
check_decision "crashing gate runs" "$decision_root/crash" true DEGRADED
check_decision "missing gate runs" "$decision_root/missing" true DEGRADED
rm -rf "$decision_root"

# The clippy adapter binds independently transported payloads to planner proofs.
# These are runtime tests of the exact script the workflow invokes; structural
# tests in test-validate-ci-gate.py separately prevent the workflow from
# bypassing it.
proof_parts() { # proof_parts <kind> <payload>
  local kind="$1" payload="$2"
  printf '%s' "$payload" | "${PAYLOAD[@]}" proof "$kind"
}

clippy_select_root="$(mktemp -d)"
check_clippy_select() { # description expected-run expected-proof expected-gate payload proof narrowed status head closure live [limit]
  local desc="$1" expected="$2"
  local expected_proof="$3" expected_gate="$4" payload="$5" proof_value="$6"
  local narrowed="$7" status="$8" head="$9" closure="${10}" live="${11}"
  local narrow_limit="${12:-}"
  local count="${proof_value%% *}" digest="${proof_value#* }"
  local output="$clippy_select_root/output"
  TESTS=$((TESTS + 1))
  : >"$output"
  if RUE_AFFECTED_NARROW_LIMIT="$narrow_limit" \
      RUE_AFFECTED_FULL=false \
      RUE_AFFECTED_LANES="$payload" \
      RUE_AFFECTED_LANES_COUNT="$count" \
      RUE_AFFECTED_LANES_DIGEST="$digest" \
      RUE_AFFECTED_NARROWED="$narrowed" \
      RUE_AFFECTED_NARROWING_STATUS="$status" \
      RUE_AFFECTED_HEAD_TARGET_COUNT="$head" \
      RUE_AFFECTED_IMPACTED_CLOSURE_COUNT="$closure" \
      RUE_AFFECTED_IMPACTED_TARGET_COUNT="$live" \
      GITHUB_OUTPUT="$output" \
      "${CLIPPY[@]}" select >/dev/null 2>&1 && \
      grep -Fxq "run=$expected" "$output" && \
      grep -Fxq "proof_status=$expected_proof" "$output" && \
      grep -Fxq "gate_status=$expected_gate" "$output"; then
    pass "clippy selection proof: $desc"
  else
    fail "clippy selection proof: $desc"
  fi
}

empty_lane_proof="$(proof_parts lanes "")"
clippy_lane_proof="$(proof_parts lanes "clippy")"
two_lane_proof="$(proof_parts lanes "clippy release")"
invalid_lane_proof="$(proof_parts lanes "clippy not-a-lane")"
check_clippy_select "missing lane payload fails open" true DEGRADED RUN "" "$clippy_lane_proof" false DECLINED 2 0 0
check_clippy_select "valid-prefix lane truncation fails open" true DEGRADED RUN "clippy" "$two_lane_proof" true CANDIDATE 3 1 1
check_clippy_select "lane payload mutation fails open" true DEGRADED RUN "release" "$clippy_lane_proof" true CANDIDATE 3 1 1
check_clippy_select "leading-zero head metadata fails open" true DEGRADED RUN "" "$empty_lane_proof" false DECLINED 00 0 0
check_clippy_select "zero-impact CANDIDATE metadata fails open" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 3 0 0
check_clippy_select "legitimate empty lane set deselects" false SELECTIVE DESELECTED "" "$empty_lane_proof" false DECLINED 3 0 0
check_clippy_select "valid selected clippy lane runs" true SELECTIVE RUN "clippy" "$clippy_lane_proof" true CANDIDATE 3 1 1
check_clippy_select "gate rejection cannot authorize narrowing" true SELECTIVE DEGRADED "clippy not-a-lane" "$invalid_lane_proof" true CANDIDATE 3 1 1
check_clippy_select "closure at the canonical limit with live impact is a candidate" true SELECTIVE RUN "clippy" "$clippy_lane_proof" true CANDIDATE 100 600 100
check_clippy_select "oversized closure with a small live set rejects candidate" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 100 601 100
check_clippy_select "oversized closure with a small live set accepts declined" true SELECTIVE RUN "clippy" "$clippy_lane_proof" false DECLINED 100 601 100
check_clippy_select "bounded closure with no live targets accepts declined" true SELECTIVE RUN "clippy" "$clippy_lane_proof" false DECLINED 100 10 0
check_clippy_select "bounded live closure cannot be falsely declined" true DEGRADED RUN "clippy" "$clippy_lane_proof" false DECLINED 100 600 100
check_clippy_select "empty-live closure cannot be a candidate" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 100 10 0
check_clippy_select "live count cannot exceed raw closure" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 3 1 2
check_clippy_select "missing closure count fails open" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 3 "" 1
check_clippy_select "malformed closure count fails open" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 3 01 1
check_clippy_select "candidate exactly at a custom canonical limit remains selective" true SELECTIVE RUN "clippy" "$clippy_lane_proof" true CANDIDATE 2 2 2 2
check_clippy_select "candidate above a custom canonical limit fails open" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 2 3 2 2
check_clippy_select "malformed canonical limit fails open" true DEGRADED RUN "clippy" "$clippy_lane_proof" true CANDIDATE 3 1 1 00

check_clippy_materialize() { # description expected-status payload proof expected-file [proof-status] [gate-status]
  local desc="$1" expected_status="$2" payload="$3" proof_value="$4" expected_file="$5"
  local proof_status="${6:-SELECTIVE}" gate_status="${7:-RUN}"
  local count="${proof_value%% *}" digest="${proof_value#* }"
  local output="$clippy_select_root/materialize-output"
  local summary="$clippy_select_root/materialize-summary"
  local file="$clippy_select_root/impacted-clippy-targets.txt"
  TESTS=$((TESTS + 1))
  : >"$output"
  : >"$summary"
  if RUNNER_TEMP="$clippy_select_root" \
      GITHUB_OUTPUT="$output" \
      GITHUB_STEP_SUMMARY="$summary" \
      RUE_CLIPPY_PROOF_STATUS="$proof_status" \
      RUE_CLIPPY_GATE_STATUS="$gate_status" \
      RUE_AFFECTED_NARROWED=true \
      RUE_AFFECTED_IMPACTED="$payload" \
      RUE_AFFECTED_IMPACTED_TARGET_COUNT="$count" \
      RUE_AFFECTED_IMPACTED_TARGETS_DIGEST="$digest" \
      "${CLIPPY[@]}" materialize >/dev/null 2>&1 && \
      grep -Fxq "status=$expected_status" "$output" && \
      cmp -s "$file" "$expected_file"; then
    pass "clippy impacted proof: $desc"
  else
    fail "clippy impacted proof: $desc"
  fi
}

: >"$clippy_select_root/empty"
clippy_first=$'//crates/one:one-clippy\n//crates/two:two-test'
non_clippy_first=$'//crates/two:two-test\n//crates/one:one-clippy'
clippy_first_proof="$(proof_parts targets "$clippy_first")"
non_clippy_first_proof="$(proof_parts targets "$non_clippy_first")"
printf '%s\n' "$clippy_first" >"$clippy_select_root/complete"
check_clippy_materialize \
  "clippy-retaining valid prefix degrades" DEGRADED \
  "//crates/one:one-clippy" "$clippy_first_proof" "$clippy_select_root/empty"
check_clippy_materialize \
  "non-clippy-retaining valid prefix degrades" DEGRADED \
  "//crates/two:two-test" "$non_clippy_first_proof" "$clippy_select_root/empty"
check_clippy_materialize \
  "complete canonical payload remains a candidate" CANDIDATE \
  "$clippy_first" "$clippy_first_proof" "$clippy_select_root/complete"
check_clippy_materialize \
  "selection proof failure forces the full inventory" DEGRADED \
  "$clippy_first" "$clippy_first_proof" "$clippy_select_root/empty" DEGRADED RUN
check_clippy_materialize \
  "gate failure forces the full inventory" DEGRADED \
  "$clippy_first" "$clippy_first_proof" "$clippy_select_root/empty" SELECTIVE DEGRADED
check_clippy_materialize \
  "authoritative full selection cannot narrow" DECLINED \
  "$clippy_first" "$clippy_first_proof" "$clippy_select_root/empty" FULL RUN

# A successful-empty canonical query is a hard error on the first narrowed
# attempt. It must not be retried as a full query that can fail and enter the
# broad passing fallback.
clippy_runner_root="$(mktemp -d)"
mkdir -p "$clippy_runner_root/scripts"
cp "$SCRIPTS_DIR/ci-clippy" "$clippy_runner_root/scripts/ci-clippy"
chmod +x "$clippy_runner_root/scripts/ci-clippy"
cat >"$clippy_runner_root/scripts/affected-targets" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$1" >>"${CLIPPY_AFFECTED_LOG:?}"
case "$1" in
  narrow-scope) exit "${CLIPPY_NARROW_RC:-0}" ;;
  scope-targets) exit "${CLIPPY_SCOPE_RC:-1}" ;;
  *) exit 2 ;;
esac
EOF
cat >"$clippy_runner_root/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${CLIPPY_BUCK_LOG:?}"
EOF
chmod +x "$clippy_runner_root/scripts/affected-targets" "$clippy_runner_root/buck2"
TESTS=$((TESTS + 1))
: >"$clippy_runner_root/affected.log"
: >"$clippy_runner_root/buck.log"
if ! NARROW_STATUS=CANDIDATE \
    NARROW_FILE="$clippy_runner_root/impacted" \
    CLIPPY_NARROW_RC=2 \
    CLIPPY_SCOPE_RC=1 \
    CLIPPY_AFFECTED_LOG="$clippy_runner_root/affected.log" \
    CLIPPY_BUCK_LOG="$clippy_runner_root/buck.log" \
    "$clippy_runner_root/scripts/ci-clippy" run >/dev/null 2>&1 && \
    [ "$(tr '\n' ' ' <"$clippy_runner_root/affected.log")" = "narrow-scope " ] && \
    [ ! -s "$clippy_runner_root/buck.log" ]; then
  pass "clippy runner: first narrow-scope rc=2 is an immediate hard error"
else
  fail "clippy runner: first narrow-scope rc=2 was retried or fell back"
fi
rm -rf "$clippy_select_root" "$clippy_runner_root"

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
mkdir -p "$integration_root/scripts" "$integration_root/bin" "$integration_root/docs"
cp "$SCRIPTS_DIR/affected-targets" \
  "$SCRIPTS_DIR/ci-affected-payload.py" \
  "$SCRIPTS_DIR/parse-btd-impacted.py" \
  "$integration_root/scripts/"
chmod +x "$integration_root/scripts/affected-targets"

cat >"$integration_root/bin/fake-buck" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "uquery" ]; then
  case "${2:-}" in
    kind*)
      printf 'root//crates/rue-span:rue-span-clippy\n' ;;
    attrfilter*)
      printf 'root//crates/rue-codegen:rue-codegen-test\n' ;;
    *) exit 1 ;;
  esac
  exit 0
fi
output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then output="$2"; shift 2; continue; fi
  shift
done
[ -n "$output" ]
printf '%s\n' '{"target":"root//:spec-tests"}' '{"target":"root//:unimpacted"}' >"$output"
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
printf '%s\n' \
  '{"target":"root//:spec-tests"}' \
  '{"target":"root//crates/deleted:base-only"}'
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
integration_selected_lanes="native-linux-arm64 native-macos-arm64"
integration_lane_proof="$(proof_parts lanes "$integration_selected_lanes")"
integration_lane_count="${integration_lane_proof%% *}"
integration_lane_digest="${integration_lane_proof#* }"
integration_impacted_proof="$(proof_parts targets "//:spec-tests")"
integration_impacted_digest="${integration_impacted_proof#* }"
if (
  cd "$integration_root" &&
  RUE_AFFECTED_BASE_SHA=HEAD~1 \
  RUE_AFFECTED_HEAD_SHA=HEAD \
  RUE_AFFECTED_BTD="$integration_root/bin/fake-btd" \
  RUE_AFFECTED_BUCK2="$integration_root/bin/fake-buck" \
  RUE_AFFECTED_BTD_ARGS="$integration_root/btd-args" \
  RUE_AFFECTED_EXPECTED_CHANGES="$integration_root/expected-changes" \
  GITHUB_OUTPUT="$integration_root/output" \
  GITHUB_STEP_SUMMARY="$integration_root/summary" \
  scripts/affected-targets decide >/dev/null
) && grep -Fxq 'full=false' "$integration_root/output" && \
    grep -Fxq 'selected=//:spec-tests' "$integration_root/output" && \
    grep -Fxq "selected_lanes=$integration_selected_lanes" "$integration_root/output" && \
    grep -Fxq "selected_lanes_count=$integration_lane_count" "$integration_root/output" && \
    grep -Fxq "selected_lanes_digest=$integration_lane_digest" "$integration_root/output" && \
    grep -Fxq 'narrowing_status=CANDIDATE' "$integration_root/output" && \
    grep -Fxq 'head_target_count=2' "$integration_root/output" && \
    grep -Fxq 'impacted_closure_count=2' "$integration_root/output" && \
    grep -Fxq 'impacted_target_count=1' "$integration_root/output" && \
    grep -Fxq "impacted_targets_digest=$integration_impacted_digest" "$integration_root/output" && \
    ! grep -Fq '//crates/deleted:base-only' "$integration_root/output" && \
    grep -Fq 'Live impacted closure: **1** targets (**50.00%' "$integration_root/summary" && \
    grep -Fq 'exact saved share is reported after each registered lane scope' "$integration_root/summary" && \
    grep -Fq 'Corpus lanes selected:' "$integration_root/summary" && \
    grep -Fxq -- '--vcs' "$integration_root/btd-args" && \
    grep -Fxq -- 'git' "$integration_root/btd-args" && \
    grep -Fxq -- '--buck' "$integration_root/btd-args" && \
    grep -Fxq -- "$integration_root/bin/fake-buck" "$integration_root/btd-args"; then
  pass "integration: BTD selects a corpus from Git status and receives pinned Buck wrapper"
else
  fail "integration: selective BTD decision contract ($(tr '\n' ' ' <"$integration_root/output"))"
fi

# Every graph-owned lane proxy is required for selective planning. If the
# canonical clippy query fails, the planner must choose the safe full run
# rather than silently omit the clippy lane.
cat >"$integration_root/bin/failing-buck" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "uquery" ]; then
  exit 1
fi
output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then output="$2"; shift 2; continue; fi
  shift
done
[ -n "$output" ]
printf '{"target":"root//:spec-tests"}\n' >"$output"
EOF
chmod +x "$integration_root/bin/failing-buck"
TESTS=$((TESTS + 1))
failure_output="$integration_root/native-query-failure-output"
if (
  cd "$integration_root" &&
  RUE_AFFECTED_BASE_SHA=HEAD~1 \
  RUE_AFFECTED_HEAD_SHA=HEAD \
  RUE_AFFECTED_BTD="$integration_root/bin/fake-btd" \
  RUE_AFFECTED_BUCK2="$integration_root/bin/failing-buck" \
  RUE_AFFECTED_BTD_ARGS="$integration_root/failure-btd-args" \
  RUE_AFFECTED_EXPECTED_CHANGES="$integration_root/expected-changes" \
  GITHUB_OUTPUT="$failure_output" \
  GITHUB_STEP_SUMMARY="$integration_root/degraded-summary" \
  scripts/affected-targets decide >/dev/null 2>&1
) && grep -Fxq 'full=true' "$failure_output" && \
    grep -Fxq 'narrowing_status=DEGRADED' "$failure_output" && \
    grep -Fq 'Planner reach: **not applicable**' "$integration_root/degraded-summary"; then
  pass "decision: clippy graph query failure runs full suite"
else
  fail "decision: clippy graph query failure did not fail open to full suite"
fi

cat >"$integration_root/bin/empty-head-buck" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "uquery" ]; then
  printf 'root//:spec-tests\n'
  exit 0
fi
output=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--output" ]; then output="$2"; shift 2; continue; fi
  shift
done
[ -n "$output" ]
: >"$output"
EOF
chmod +x "$integration_root/bin/empty-head-buck"
TESTS=$((TESTS + 1))
empty_head_output="$integration_root/empty-head-output"
if (
  cd "$integration_root" &&
  RUE_AFFECTED_BASE_SHA=HEAD~1 \
  RUE_AFFECTED_HEAD_SHA=HEAD \
  RUE_AFFECTED_BTD="$integration_root/bin/fake-btd" \
  RUE_AFFECTED_BUCK2="$integration_root/bin/empty-head-buck" \
  RUE_AFFECTED_BTD_ARGS="$integration_root/empty-head-btd-args" \
  RUE_AFFECTED_EXPECTED_CHANGES="$integration_root/expected-changes" \
  GITHUB_OUTPUT="$empty_head_output" \
  GITHUB_STEP_SUMMARY="$integration_root/empty-head-summary" \
  scripts/affected-targets decide >/dev/null 2>&1
) && grep -Fxq 'full=true' "$empty_head_output" && \
    grep -Fxq 'narrowing_status=DEGRADED' "$empty_head_output"; then
  pass "decision: an empty successful head dump degrades to full"
else
  fail "decision: an empty successful head dump was treated as selective"
fi

TESTS=$((TESTS + 1))
full_output="$integration_root/full-output"
full_summary="$integration_root/full-summary"
if (
  cd "$integration_root" &&
  RUE_AFFECTED_EVENT=merge_group \
  GITHUB_OUTPUT="$full_output" \
  GITHUB_STEP_SUMMARY="$full_summary" \
  scripts/affected-targets decide >/dev/null 2>&1
) && grep -Fxq 'full=true' "$full_output" && \
    grep -Fxq 'narrowing_status=DECLINED' "$full_output" && \
    grep -Fq 'Planner reach: **not applicable**' "$full_summary"; then
  pass "decision: authoritative full run reports no saved share"
else
  fail "decision: authoritative full run fabricated a saved share"
fi

TESTS=$((TESTS + 1))
dispatch_output="$integration_root/dispatch-output"
dispatch_summary="$integration_root/dispatch-summary"
if (
  cd "$integration_root" &&
  RUE_AFFECTED_EVENT=workflow_dispatch \
  GITHUB_OUTPUT="$dispatch_output" \
  GITHUB_STEP_SUMMARY="$dispatch_summary" \
  scripts/affected-targets decide >/dev/null 2>&1
) && grep -Fxq 'full=true' "$dispatch_output" && \
    grep -Fxq 'narrowing_status=DECLINED' "$dispatch_output" && \
    grep -Fq "authoritative full run for event 'workflow_dispatch'" "$dispatch_summary"; then
  pass "decision: workflow_dispatch remains an authoritative full run"
else
  fail "decision: workflow_dispatch did not force the full suite"
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
