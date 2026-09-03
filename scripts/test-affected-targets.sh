#!/usr/bin/env bash
# test-affected-targets.sh — regression tests for the coverage-deciding logic
# of pull-request selection (RUE-1119): the out-of-graph force-full matcher and
# decision in scripts/affected-targets, the fail-open gate in
# scripts/ci-corpus-selected, the clippy runner, the build scope's corpus-action
# deferral (RUE-1511), strict BTD/dump decoding, and one end-to-end decision
# with fake git, BTD, and `buck2 targets`. Everything that would consult the
# graph goes through RUE_AFFECTED_BUCK2: this suite runs as an sh_test under
# buck2, which refuses a nested query, and on macOS under Bash 3.2.
set -uo pipefail

if [ -n "${RUE_AFFECTED_SCRIPTS_ROOT:-}" ]; then
  SCRIPTS_DIR="$RUE_AFFECTED_SCRIPTS_ROOT/scripts"
else
  SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"
fi
REPO_ROOT="$(cd "$SCRIPTS_DIR/.." && pwd)"
# Via `bash`: the execute bit need not survive Buck resource materialization.
AFFECTED=(bash "$SCRIPTS_DIR/affected-targets")
GATE=(bash "$SCRIPTS_DIR/ci-corpus-selected")
PARSER=(python3 "$SCRIPTS_DIR/parse-btd-impacted.py")

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
FAILURES=0
TESTS=0
fail() { printf 'FAIL: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }
pass() { printf 'ok: %s\n' "$1"; }
check() { # check <desc> <expected> <actual>
  TESTS=$((TESTS + 1))
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (expected '$2', got '$3')"; fi
}
fake() { # fake <path> <body...>: an executable stub
  printf '#!/usr/bin/env bash\n%s\n' "$2" >"$1"
  chmod +x "$1"
}

# --- out-of-graph / graph-global changes MUST force a full run --------------
for path in BUCK crates/rue-air/BUCK toolchains/rust/defs.bzl prelude/some.bzl .buckconfig \
    .buckconfig.local.example .buckroot .github/workflows/ci.yml toolchains/rust/BUCK platforms/BUCK \
    constraints/BUCK third-party/rust/Cargo.toml rust-toolchain.toml buck2 buck2-bin reindeer test.sh \
    scripts/ci-heavy-suite scripts/ci-timed scripts/ci-corpus-selected scripts/ci-clippy \
    scripts/affected-targets btd scripts/provision-build-cache scripts/install-valgrind scripts/rue \
    scripts/rue-bin crates/rue-runtime-asan/src/main.rs crates/rue-runtime-asan/Cargo.toml; do
  TESTS=$((TESTS + 1))
  if printf '%s\n' "$path" | "${AFFECTED[@]}" force-full-match >/dev/null; then pass "force-full: $path"; else fail "force-full expected: $path"; fi
done
# --- peripheral / in-crate changes are eligible (BTD decides their reach) ---
for path in crates/rue-air/src/lib.rs crates/rue-spec/cases/4.2/example.rue docs/process/logging.md \
    website/src/index.md examples/hello/main.rue README.md website/build.sh; do
  TESTS=$((TESTS + 1))
  if printf '%s\n' "$path" | "${AFFECTED[@]}" force-full-match >/dev/null; then fail "unexpectedly force-full: $path"; else pass "selectable: $path"; fi
done
TESTS=$((TESTS + 1))
if printf '%s\n' crates/rue-air/src/lib.rs toolchains/rust/defs.bzl | "${AFFECTED[@]}" force-full-match >/dev/null; then
  pass "force-full: mixed diff with one graph-global path"
else fail "force-full: mixed diff with a .bzl change"; fi

# --- ci-corpus-selected: run=false only on an explicit selective miss --------
gate() { # gate <full> <targets> <lanes> <unit> -> run=...
  local out="$WORK/gate-output"
  : >"$out"
  RUE_AFFECTED_FULL="$1" RUE_AFFECTED_TARGETS="$2" RUE_AFFECTED_LANES="$3" GITHUB_OUTPUT="$out" "${GATE[@]}" "$4" >/dev/null
  cat "$out"
}
check "gate: full=true runs a corpus" run=true "$(gate true "" "" //:spec-tests)"
check "gate: selected corpus runs" run=true "$(gate false "//:spec-tests //:cli-tests-shard-0" "" //:spec-tests)"
check "gate: unselected corpus is deselected" run=false "$(gate false "//:cli-tests-shard-0" "" //:spec-tests)"
check "gate: empty selective selection deselects" run=false "$(gate false "" "" //:spec-tests)"
check "gate: unset decision runs (fail-open)" run=true "$(gate "" "" "" //:spec-tests)"
check "gate: malformed decision runs (fail-open)" run=true "$(gate maybe "" "" //:spec-tests)"
check "gate: exact match, not substring" run=false "$(gate false "//:cli-tests-shard-10" "" //:cli-tests-shard-1)"
check "gate: selected lane runs" run=true "$(gate false "" "valgrind asan" valgrind)"
check "gate: unselected lane is deselected" run=false "$(gate false "" asan valgrind)"
check "gate: a lane reads the lane list, not the corpus list" run=false "$(gate false release "" release)"
check "gate: a corpus reads the corpus list, not the lane list" run=false "$(gate false "" //:spec-tests //:spec-tests)"
check "gate: stdout carries the decision without GITHUB_OUTPUT" run=true "$(RUE_AFFECTED_FULL=true "${GATE[@]}" asan)"
TESTS=$((TESTS + 1))
if "${GATE[@]}" >/dev/null 2>&1; then fail "gate: missing unit accepted"; else pass "gate: missing unit is a usage error"; fi

# --- lanes and their live-graph proxies ------------------------------------
fake "$WORK/graph-buck" 'case "${2:-}" in
  kind*) printf "%s\n" root//crates/rue-codegen:rue-codegen-clippy root//crates/rue-codegen:rue-codegen-debug-assert-check ;;
  *rue_ci_clippy_lane*) printf "%s\n" root//crates/rue-codegen:rue-codegen-clippy ;;
  *) printf "%s\n" root//crates/rue-codegen:rue-codegen-test ;;
esac'
export RUE_AFFECTED_BUCK2="$WORK/graph-buck"
check "lanes: the gated inventory" "clippy native-linux-arm64 native-macos-arm64 release valgrind asan compiler-reproducibility rue-program-digests" "$("${AFFECTED[@]}" lanes | tr '\n' ' ' | sed 's/ $//')"
for lane in $("${AFFECTED[@]}" lanes); do
  TESTS=$((TESTS + 1))
  if [ -n "$("${AFFECTED[@]}" lane-targets "$lane")" ]; then pass "lane: $lane declares targets"; else fail "lane: $lane declares no targets (would always deselect)"; fi
done
TESTS=$((TESTS + 1))
if "${AFFECTED[@]}" lane-targets not-a-lane >/dev/null 2>&1; then fail "lane: unknown lane accepted"; else pass "lane: unknown lane is an error (decide runs full)"; fi
check "native: lane proxies are the graph plus the two corpora" "//crates/rue-codegen:rue-codegen-test //:spec-tests //:cli-tests" "$("${AFFECTED[@]}" lane-targets native-macos-arm64 | tr '\n' ' ' | sed 's/ $//')"
check "clippy: owner-label query" "//crates/rue-codegen:rue-codegen-clippy" "$("${AFFECTED[@]}" clippy-owned-targets)"
check "clippy: lane proxy equals the runnable scope" "$("${AFFECTED[@]}" scope-targets clippy)" "$("${AFFECTED[@]}" lane-targets clippy)"
TESTS=$((TESTS + 1))
if "${AFFECTED[@]}" scope-targets future-lane >/dev/null 2>&1; then fail "narrow: unknown consumer accepted"; else pass "narrow: unknown consumer is rejected"; fi

# --- narrow-scope: an exact intersection with the live scope ---------------
printf '%s\n' //crates/rue-codegen:rue-codegen-test //crates/rue-other:other-test >"$WORK/native-impacted"
check "narrow: native output is scope ∩ impacted" "//crates/rue-codegen:rue-codegen-test" "$("${AFFECTED[@]}" narrow-scope native-platforms-units "$WORK/native-impacted" 2>/dev/null)"
TESTS=$((TESTS + 1))
if RUE_AFFECTED_BUCK2="$WORK/absent" "${AFFECTED[@]}" narrow-scope native-platforms-units "$WORK/native-impacted" >/dev/null 2>&1; then
  fail "narrow: unavailable graph reported a verified subset"; else pass "narrow: unavailable graph is not a verified subset"; fi
printf '%s\n' //crates/rue-codegen:rue-codegen-clippy //crates/removed:removed-clippy //crates/rue-codegen:rue-codegen-debug-assert-check >"$WORK/clippy-impacted"
check "clippy: narrowed scope keeps only live impacted -clippy targets" "//crates/rue-codegen:rue-codegen-clippy" "$("${AFFECTED[@]}" narrow-scope clippy "$WORK/clippy-impacted" 2>/dev/null)"
printf '%s\n' //crates/removed:removed-clippy >"$WORK/stale-only"
check "clippy: stale-only impact is a verified empty subset" "0:" "$("${AFFECTED[@]}" narrow-scope clippy "$WORK/stale-only" 2>/dev/null; echo "$?:")"
: >"$WORK/empty"
printf 'not-a-target\n' >"$WORK/malformed"
for bad in empty malformed absent; do
  TESTS=$((TESTS + 1))
  if "${AFFECTED[@]}" narrow-scope clippy "$WORK/$bad" >/dev/null 2>&1; then fail "narrow: $bad impacted file masqueraded as verified"; else pass "narrow: $bad impacted file degrades to the full scope"; fi
done
fake "$WORK/clippy-empty-buck" 'printf "%s\n" root//crates/rue-alpha:rue-alpha-debug-assert-check'
check "clippy: successful empty live inventory is the distinct hard error (2)" 2 "$(RUE_AFFECTED_BUCK2="$WORK/clippy-empty-buck" "${AFFECTED[@]}" scope-targets clippy >/dev/null 2>&1; echo $?)"
check "clippy: that status survives narrowing" 2 "$(RUE_AFFECTED_BUCK2="$WORK/clippy-empty-buck" "${AFFECTED[@]}" narrow-scope clippy "$WORK/clippy-impacted" >/dev/null 2>&1; echo $?)"
check "clippy: a failed live query is the fail-open status (1)" 1 "$(RUE_AFFECTED_BUCK2="$WORK/absent" "${AFFECTED[@]}" scope-targets clippy >/dev/null 2>&1; echo $?)"

# --- ci-clippy run: hard error, narrowed, no-op, and fail-open paths --------
mkdir -p "$WORK/clippy-root/scripts"
cp "$SCRIPTS_DIR/ci-clippy" "$WORK/clippy-root/scripts/ci-clippy"
fake "$WORK/clippy-root/scripts/affected-targets" 'printf "%s\n" "$1" >>"$CLIPPY_LOG"
case "$1" in
  narrow-scope) printf "%s" "${CLIPPY_NARROW_OUT:-}"; exit "${CLIPPY_NARROW_RC:-0}" ;;
  scope-targets) printf "%s" "${CLIPPY_SCOPE_OUT:-}"; exit "${CLIPPY_SCOPE_RC:-0}" ;;
  *) exit 2 ;;
esac'
fake "$WORK/clippy-root/buck2" 'printf "%s\n" "$*" >>"$CLIPPY_LOG"'
chmod +x "$WORK/clippy-root/scripts/ci-clippy"
run_clippy() { # run_clippy <VAR=VALUE...> -> "<rc>|<log lines joined by ;>"
  local rc
  : >"$WORK/clippy-log"
  CLIPPY_LOG="$WORK/clippy-log" env "$@" "$WORK/clippy-root/scripts/ci-clippy" run >/dev/null 2>&1
  rc=$?
  printf '%s|%s' "$rc" "$(tr '\n' ';' <"$WORK/clippy-log")"
}
check "clippy run: empty live inventory on the first narrowed query is an immediate hard error" "1|narrow-scope;" \
  "$(run_clippy NARROW_FILE="$WORK/clippy-impacted" CLIPPY_NARROW_RC=2)"
check "clippy run: a verified narrowed subset is what runs" "0|narrow-scope;test //crates/a:a-clippy;" \
  "$(run_clippy NARROW_FILE="$WORK/clippy-impacted" CLIPPY_NARROW_OUT=$'//crates/a:a-clippy\n')"
check "clippy run: a verified empty subset is an intentional no-op" "0|narrow-scope;" \
  "$(run_clippy NARROW_FILE="$WORK/clippy-impacted" CLIPPY_NARROW_OUT="")"
check "clippy run: narrowing failure falls back to the full live inventory" "0|narrow-scope;scope-targets;test //crates/a:a-clippy;" \
  "$(run_clippy NARROW_FILE="$WORK/clippy-impacted" CLIPPY_NARROW_RC=1 CLIPPY_SCOPE_OUT=$'//crates/a:a-clippy\n')"
check "clippy run: no impacted list runs the full live inventory" "0|scope-targets;test //crates/a:a-clippy;" \
  "$(run_clippy NARROW_FILE="$WORK/empty" CLIPPY_SCOPE_OUT=$'//crates/a:a-clippy\n')"
check "clippy run: a failed live query runs the broad superset" "0|scope-targets;test //crates/...;" \
  "$(run_clippy CLIPPY_SCOPE_RC=1)"
check "clippy run: an empty full inventory stays a hard error" "1|scope-targets;" "$(run_clippy CLIPPY_SCOPE_RC=2)"

# --- build scope: the premerge build must not RUN a corpus (RUE-1511) --------
# `cached_corpus_suite` splits a corpus into an action that runs the harness
# and a stamp test; building the action executes the corpus. The scope is
# `//crates/...` minus rdeps of every `_corpus_action` a required lane owns,
# where ownership comes from deps(premerge selection ∪ platform-corpus set).
# `newthing-test-action` is owned by no lane and must stay in the build.
fake "$WORK/scope-buck" 'case "$1" in
  uquery)
    case "$2" in
      "kind('"'"'_corpus_action'"'"', //...)") printf "%s\n" root//:spec-tests-action root//crates/rue-oracle-diff:oracle-diff-test-action ;;
      "attrfilter(labels, '"'"'rue_heavy_suite'"'"', //...)"*) printf "%s\n" root//:spec-tests root//crates/rue-oracle-diff:oracle-diff-test ;;
      "attrfilter(labels, rue_test_tier_premerge, //crates/...)"*) printf "%s\n" root//crates/rue-span:rue-span-test ;;
      "//crates/... except set("*)
        case "$2" in
          *"//crates/rue-oracle-diff:oracle-diff-test-action"*"//crates/rue-oracle-diff:corpus-carrier"*)
            printf "%s\n" root//crates/rue-compiler:rue-compiler root//crates/rue-newthing:newthing-test-action root//crates/rue-span:rue-span-test ;;
          *) echo "scope-buck: except set lost a deferred target: $2" >&2; exit 1 ;;
        esac ;;
      *) echo "scope-buck: unexpected uquery: $2" >&2; exit 1 ;;
    esac ;;
  cquery)
    case "$2" in
      "rdeps(//crates/..., kind('"'"'_corpus_action'"'"', deps(set("*"//crates/rue-oracle-diff:oracle-diff-test"*)
        printf "%s\n" "root//crates/rue-oracle-diff:oracle-diff-test-action (linux)" "root//crates/rue-oracle-diff:oracle-diff-test (linux)" "root//crates/rue-oracle-diff:corpus-carrier (linux)" ;;
      *) echo "scope-buck: unexpected cquery: $2" >&2; exit 1 ;;
    esac ;;
  *) exit 1 ;;
esac'
check "scope: unnarrowed build scope excludes the owned closure and keeps the unowned action" \
  "//crates/rue-compiler:rue-compiler //crates/rue-newthing:newthing-test-action //crates/rue-span:rue-span-test" \
  "$(RUE_AFFECTED_BUCK2="$WORK/scope-buck" "${AFFECTED[@]}" scope-targets linux-premerge-build 2>/dev/null | tr '\n' ' ' | sed 's/ $//')"
printf '%s\n' //crates/rue-compiler:rue-compiler //:cli-tests //:cli-tests-action //crates/rue-codegen:rue-codegen-test \
  //:spec-tests-action //crates/rue-oracle-diff:oracle-diff-test-action //crates/rue-oracle-diff:corpus-carrier >"$WORK/mixed"
check "scope: narrowed build scope is the exact impacted ∩ live-scope intersection" "//crates/rue-compiler:rue-compiler" \
  "$(RUE_AFFECTED_BUCK2="$WORK/scope-buck" "${AFFECTED[@]}" narrow-scope linux-premerge-build "$WORK/mixed" 2>/dev/null)"
printf '%s\n' //crates/rue-oracle-diff:oracle-diff-test-action //crates/rue-newthing:newthing-test-action //crates/rue-span:rue-span-test >"$WORK/unowned"
check "scope: a corpus action no lane owns survives narrowing" "//crates/rue-newthing:newthing-test-action //crates/rue-span:rue-span-test" \
  "$(RUE_AFFECTED_BUCK2="$WORK/scope-buck" "${AFFECTED[@]}" narrow-scope linux-premerge-build "$WORK/unowned" 2>/dev/null | tr '\n' ' ' | sed 's/ $//')"
printf '%s\n' //:cli-tests-action //:spec-tests-action >"$WORK/corpora-only"
check "scope: a corpus-only closure is a verified empty build (nothing to build)" "0:" \
  "$(RUE_AFFECTED_BUCK2="$WORK/scope-buck" "${AFFECTED[@]}" narrow-scope linux-premerge-build "$WORK/corpora-only" 2>/dev/null; echo "$?:")"
check "scope: the premerge test scope narrows to nothing on a corpus-only closure" "0:" \
  "$(RUE_AFFECTED_BUCK2="$WORK/graph-buck" "${AFFECTED[@]}" narrow-scope linux-premerge-tests "$WORK/corpora-only" 2>/dev/null; echo "$?:")"
fake "$WORK/closure-fail-buck" '[ "$1" = cquery ] && exit 1; exec "'"$WORK/scope-buck"'" "$@"'
for buck in absent closure-fail-buck; do
  TESTS=$((TESTS + 1))
  if RUE_AFFECTED_BUCK2="$WORK/$buck" "${AFFECTED[@]}" scope-targets linux-premerge-build >/dev/null 2>&1; then
    fail "scope: $buck produced a guessed scope"; else pass "scope: $buck fails so the workflow builds the full pattern"; fi
done

# --- test.sh reads the narrowed list on Bash 3.2 (RUE-1506) -----------------
run_test_sh_args() { # run_test_sh_args [VAR=VALUE ...] -> the buck2 test args
  mkdir -p "$WORK/shim"
  printf '#!/usr/bin/env bash\nshift\n[ "${1:-}" = test ] && { shift; printf "ARGS: %%s\\n" "$*"; exit 0; }\nexit 0\n' >"$WORK/shim/dotslash"
  chmod +x "$WORK/shim/dotslash"
  ( cd "$REPO_ROOT" && PATH="$WORK/shim:$PATH" CI=true RUE_TEST_TIER=premerge env "$@" ./test.sh 2>&1 ) | grep '^ARGS: ' || true
}
check_narrow() { # check_narrow <desc> <expect-substring> [VAR=VALUE ...]
  local desc="$1" expect="$2" actual; shift 2
  actual="$(run_test_sh_args "$@")"
  TESTS=$((TESTS + 1))
  case "$actual" in *"$expect"*) pass "test.sh: $desc" ;; *) fail "test.sh: $desc (got '$actual')" ;; esac
}
printf '//crates/rue-span:rue-span-test\n' >"$WORK/one"
printf '//crates/rue-span:rue-span-test\n\n//crates/rue-parser:rue-parser-test\n' >"$WORK/blank-line"
printf '//crates/rue-span:rue-span-test' >"$WORK/unterminated"
printf '   \n\t\n' >"$WORK/whitespace"
mkdir -p "$WORK/a-directory"
check_narrow "a readable list narrows the suite" "ARGS: //crates/rue-span:rue-span-test --always-exclude" "RUE_TEST_TARGETS_FILE=$WORK/one"
check_narrow "a narrowed suite keeps its tier and deferral filters" "--include rue_test_tier_premerge --exclude rue_ci_dedicated_lane" "RUE_TEST_TARGETS_FILE=$WORK/one"
check_narrow "blank lines are not passed to buck2" "ARGS: //crates/rue-span:rue-span-test //crates/rue-parser:rue-parser-test --always-exclude" "RUE_TEST_TARGETS_FILE=$WORK/blank-line"
check_narrow "a final line without a newline is still a target" "ARGS: //crates/rue-span:rue-span-test --always-exclude" "RUE_TEST_TARGETS_FILE=$WORK/unterminated"
for bad in empty whitespace absent a-directory; do
  check_narrow "a(n) $bad list runs the full pattern" "ARGS: //... toolchains//..." "RUE_TEST_TARGETS_FILE=$WORK/$bad"
done
check_narrow "an unset list runs the full pattern" "ARGS: //... toolchains//..."
check "test.sh: a verified empty test scope runs no tests" "" "$(run_test_sh_args "RUE_TEST_TARGETS_FILE=$WORK/empty" RUE_TEST_TARGETS_STATUS=VERIFIED)"

# --- strict BTD / targets-dump decoding: the two shapes are not interchangeable
parse() { printf '%b' "$2" | "${PARSER[@]}" $1 2>/dev/null || echo "<rc=$?>"; }
check "parser: empty stream is valid" "" "$(parse '' '')"
check "parser: normalizes the root cell" "//:spec-tests" "$(parse '' '{"target":"root//:spec-tests"}\n')"
check "parser: partially malformed stream fails" "<rc=1>" "$(parse '' '{"target":"root//:spec-tests"}\nnot-json\n')"
check "parser: missing target fails" "<rc=1>" "$(parse '' '{}\n')"
check "parser: non-string target fails" "<rc=1>" "$(parse '' '{"target":7}\n')"
check "parser: a dump-shaped record is rejected by the BTD mode" "<rc=1>" "$(parse '' '{"buck.package":"root//","name":"spec-tests"}\n')"
check "dump parser: root package joins without a separator" "//:spec-tests" "$(parse --targets-dump '{"buck.package":"root//","name":"spec-tests","buck.type":"prelude//rules.bzl:sh_test"}\n')"
check "dump parser: crate package joins with a colon" "//crates/rue:rue" "$(parse --targets-dump '{"buck.package":"root//crates/rue","name":"rue"}\n')"
check "dump parser: duplicates collapse in first-seen order" $'//:b\n//:a' "$(parse --targets-dump '{"buck.package":"root//","name":"b"}\n{"buck.package":"root//","name":"a"}\n{"buck.package":"root//","name":"b"}\n')"
check "dump parser: a --imports package record is not a target" "//:spec-tests" "$(parse --targets-dump '{"buck.package":"root//","buck.file":"root//BUCK","buck.imports":["prelude//prelude.bzl"]}\n{"buck.package":"root//","name":"spec-tests"}\n')"
check "dump parser: a BTD-shaped record is rejected" "<rc=1>" "$(parse --targets-dump '{"target":"root//:spec-tests"}\n')"
check "dump parser: missing name fails" "<rc=1>" "$(parse --targets-dump '{"buck.package":"root//"}\n')"
check "dump parser: a --keep-going package error fails open" "<rc=1>" "$(parse --targets-dump '{"buck.package":"root//broken","buck.error":"parse error"}\n')"
check "parser: unknown flag is a usage error" "<rc=2>" "$(parse --no-such-flag '')"

# --- end to end: git status + BTD + real-shaped `buck2 targets` dump ---------
E="$WORK/e2e"
mkdir -p "$E/scripts" "$E/bin" "$E/docs"
cp "$SCRIPTS_DIR/affected-targets" "$SCRIPTS_DIR/parse-btd-impacted.py" "$E/scripts/"
fake "$E/bin/fake-buck" 'set -euo pipefail
if [ "${1:-}" = uquery ]; then
  case "${2:-}" in
    "kind('"'"'sh_test'"'"'"*) echo root//crates/rue-span:rue-span-clippy ;;
    "kind('"'"'_corpus_action'"'"'"*) echo root//:spec-tests-action ;;
    "attrfilter(labels, '"'"'rue_heavy_suite'"'"'"*) echo root//:spec-tests ;;
    "attrfilter(labels, '"'"'rue_platform_native'"'"'"*) echo root//crates/rue-codegen:rue-codegen-test ;;
    *) exit 1 ;;
  esac
  exit 0
fi
output=""
while [ "$#" -gt 0 ]; do if [ "$1" = --output ]; then output="$2"; shift 2; continue; fi; shift; done
[ -n "$output" ]
# `buck2 targets --json-lines` records: package + name, never a "target" key.
# A BTD-shaped fake here hid the head-graph parse bug while CI fell open to
# FULL on every pull request.
printf "%s\n" "{\"buck.package\":\"root//\",\"name\":\"spec-tests\",\"buck.type\":\"prelude//rules.bzl:sh_test\"}" \
  "{\"buck.package\":\"root//\",\"name\":\"unimpacted\",\"buck.type\":\"prelude//rules.bzl:sh_test\"}" >"$output"'
fake "$E/bin/fake-btd" 'set -euo pipefail
printf "%s\n" "$@" >"$RUE_AFFECTED_BTD_ARGS"
changes=""
while [ "$#" -gt 0 ]; do if [ "$1" = --changes ]; then changes="$2"; shift 2; continue; fi; shift; done
cmp -s "$changes" "$RUE_AFFECTED_EXPECTED_CHANGES"
printf "%s\n" "{\"target\":\"root//:spec-tests\"}" "{\"target\":\"root//crates/deleted:base-only\"}"'
git -C "$E" init -q
git -C "$E" config user.email tests@example.invalid
git -C "$E" config user.name affected-targets-test
printf 'before\n' >"$E/docs/input.txt"
git -C "$E" add . && git -C "$E" commit -qm base
printf 'after\n' >"$E/docs/input.txt"
git -C "$E" add docs/input.txt && git -C "$E" commit -qm head
printf 'M\tdocs/input.txt\n' >"$E/expected-changes"
decide() { # decide <output-file> [VAR=VALUE ...] -> exit status of decide
  local out="$1"; shift
  : >"$out"
  ( cd "$E" && RUE_AFFECTED_BASE_SHA=HEAD~1 RUE_AFFECTED_HEAD_SHA=HEAD \
      RUE_AFFECTED_BTD="$E/bin/fake-btd" RUE_AFFECTED_BUCK2="$E/bin/fake-buck" \
      RUE_AFFECTED_BTD_ARGS="$E/btd-args" RUE_AFFECTED_EXPECTED_CHANGES="$E/expected-changes" \
      GITHUB_OUTPUT="$out" GITHUB_STEP_SUMMARY="$E/summary" env "$@" scripts/affected-targets decide >/dev/null 2>&1 )
}
output_value() { sed -n "s/^$2=//p" "$1"; }
decide "$E/out"
check "e2e: a docs-only diff is a selective decision" false "$(output_value "$E/out" full)"
check "e2e: the impacted corpus is selected" "//:spec-tests" "$(output_value "$E/out" selected)"
check "e2e: lanes whose proxies are impacted are selected" "native-linux-arm64 native-macos-arm64" "$(output_value "$E/out" selected_lanes)"
check "e2e: a small live closure is narrowed" true "$(output_value "$E/out" narrowed)"
check "e2e: the published closure is the live head intersection only" "impacted<<RUE_EOF //:spec-tests RUE_EOF" \
  "$(sed -n '/^impacted<</,/^RUE_EOF/p' "$E/out" | sed 's/RUE_EOF_[0-9]*/RUE_EOF/' | tr '\n' ' ' | sed 's/ $//')"
check "e2e: btd receives the git vcs and the pinned buck wrapper" "yes" \
  "$(if grep -Fxq -- --vcs "$E/btd-args" && grep -Fxq -- git "$E/btd-args" && grep -Fxq -- "$E/bin/fake-buck" "$E/btd-args"; then echo yes; fi)"
check "e2e: an oversized closure declines narrowing but stays selective" "false false" \
  "$(decide "$E/out-limit" RUE_AFFECTED_NARROW_LIMIT=1; echo "$(output_value "$E/out-limit" full) $(output_value "$E/out-limit" narrowed)")"
fake "$E/bin/failing-buck" 'if [ "${1:-}" = uquery ]; then exit 1; fi; exec "'"$E/bin/fake-buck"'" "$@"'
check "e2e: a failed live lane query runs the full suite" true "$(decide "$E/out-fail" RUE_AFFECTED_BUCK2="$E/bin/failing-buck"; output_value "$E/out-fail" full)"
fake "$E/bin/empty-head-buck" 'if [ "${1:-}" = uquery ]; then exec "'"$E/bin/fake-buck"'" "$@"; fi
output=""; while [ "$#" -gt 0 ]; do if [ "$1" = --output ]; then output="$2"; shift 2; continue; fi; shift; done; : >"$output"'
check "e2e: an empty successful head dump runs the full suite" true "$(decide "$E/out-empty" RUE_AFFECTED_BUCK2="$E/bin/empty-head-buck"; output_value "$E/out-empty" full)"
check "e2e: merge_group is an authoritative full run" true "$(decide "$E/out-mg" RUE_AFFECTED_EVENT=merge_group; output_value "$E/out-mg" full)"
check "e2e: workflow_dispatch is an authoritative full run" true "$(decide "$E/out-wd" RUE_AFFECTED_EVENT=workflow_dispatch; output_value "$E/out-wd" full)"
printf 'x\n' >"$E/BUCK" && git -C "$E" add BUCK && git -C "$E" commit -qm graph-global
check "e2e: a graph-global path forces a full run before any tool runs" "true " \
  "$(decide "$E/out-force" RUE_AFFECTED_BTD="$E/absent"; echo "$(output_value "$E/out-force" full) $(output_value "$E/out-force" selected)")"

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "=== affected-targets tests: PASSED ($TESTS checks) ==="
else
  echo "=== affected-targets tests: FAILED ($FAILURES/$TESTS checks) ===" >&2
  exit 1
fi
