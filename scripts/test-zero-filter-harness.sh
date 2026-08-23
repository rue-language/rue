#!/usr/bin/env bash
# End-to-end regression for explicit libtest name filters that select no cases.
# Each runner must reject the filter before it can report a zero-test pass.
set -uo pipefail

failures=0
checks=0

check() {
  checks=$((checks + 1))
  if [ "$2" -eq 0 ]; then
    printf 'ok: %s\n' "$1"
  else
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
  fi
}

run_zero_filter() {
  name="$1"
  shift
  output=''
  rc=0
  output="$("$@" rue1739_filter_must_not_match 2>&1)" || rc=$?
  check "$name rejects an unmatched explicit filter" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "$name explains that no tests matched" "$(grep -q 'no tests matched' <<<"$output" && echo 0 || echo 1)"
}

run_zero_filter spec env \
  RUE_BINARY="${RUE_BINARY:?RUE_BINARY is required}" \
  RUE_SPEC_CASES="${RUE_SPEC_CASES:?RUE_SPEC_CASES is required}" \
  "${RUE_SPEC_HARNESS:?RUE_SPEC_HARNESS is required}"
run_zero_filter ui env \
  RUE_BINARY="${RUE_BINARY:?RUE_BINARY is required}" \
  RUE_UI_CASES="${RUE_UI_CASES:?RUE_UI_CASES is required}" \
  "${RUE_UI_HARNESS:?RUE_UI_HARNESS is required}"
run_zero_filter cli env \
  RUE_BINARY="${RUE_BINARY:?RUE_BINARY is required}" \
  RUE_CLI_CASES="${RUE_CLI_CASES:?RUE_CLI_CASES is required}" \
  RUE_EXAMPLES_DIR="${RUE_EXAMPLES_DIR:?RUE_EXAMPLES_DIR is required}" \
  RUE_REPO_DIR="${RUE_REPO_DIR:?RUE_REPO_DIR is required}" \
  RUE_STD_DIR="${RUE_STD_DIR:?RUE_STD_DIR is required}" \
  "${RUE_CLI_HARNESS:?RUE_CLI_HARNESS is required}"

printf '%s\n' '--------------------------------------------------'
if [ "$failures" -eq 0 ]; then
  printf 'zero-filter harness tests: all %s checks passed\n' "$checks"
  exit 0
fi
printf 'zero-filter harness tests: %s of %s checks FAILED\n' "$failures" "$checks" >&2
exit 1
