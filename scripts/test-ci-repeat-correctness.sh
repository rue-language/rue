#!/usr/bin/env bash
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/scripts" "$tmp/logs"
cp "$root/scripts/ci-repeat-correctness" "$tmp/scripts/"
chmod +x "$tmp/scripts/ci-repeat-correctness"
cat >"$tmp/scripts/ci-heavy-suite" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$RUE_CORRECTNESS_REPETITION" >>"${FAKE_CALLS:?}"
[[ "$RUE_CORRECTNESS_REPETITION" != "${FAKE_FAIL_ON:-}" ]]
EOF
chmod +x "$tmp/scripts/ci-heavy-suite"

status=0
(cd "$tmp" && FAKE_CALLS="$tmp/calls" FAKE_FAIL_ON=1 RUE_REPETITION_LOG_DIR="$tmp/logs" \
    scripts/ci-repeat-correctness //:cli-tests-shard-0 3) >/dev/null 2>&1 || status=$?
[[ "$status" -ne 0 ]]
[[ "$(wc -l <"$tmp/calls" | tr -d ' ')" = 3 ]]
grep -Fq $'1\tFAIL' "$tmp/logs/results.tsv"
grep -Fq $'2\tPASS' "$tmp/logs/results.tsv"

: >"$tmp/calls"
rm -rf "$tmp/logs"
status=0
(cd "$tmp" && FAKE_CALLS="$tmp/calls" RUE_REPETITION_LOG_DIR="$tmp/logs" \
    scripts/ci-repeat-correctness //:cli-tests-shard-1 2) >/dev/null 2>&1 || status=$?
[[ "$status" -eq 0 ]]
[[ "$(wc -l <"$tmp/calls" | tr -d ' ')" = 2 ]]
