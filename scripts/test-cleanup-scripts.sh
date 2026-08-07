#!/usr/bin/env bash
# test-cleanup-scripts.sh — fail-closed regression tests for maintenance
# scripts jj-tidy and rue-storage (RUE-567, RUE-1225).
#
# jj-tidy performs remote branch deletion. rue-storage only deletes rebuildable
# Buck outputs, but it still refuses to infer targets from a failed inventory.
#
# Each test runs a copy of the real script in a throwaway sandbox with fake
# tools, so no real repo, remote, or Buck output is touched.
set -uo pipefail

# Directory holding the scripts under test. Under buck2 sh_test this is the
# materialized `:cleanup-script-inputs` filegroup (RUE_CLEANUP_SCRIPTS_ROOT);
# run directly from a checkout it defaults to the repo's scripts/ dir.
if [ -n "${RUE_CLEANUP_SCRIPTS_ROOT:-}" ]; then
  SCRIPTS_DIR="$RUE_CLEANUP_SCRIPTS_ROOT/scripts"
else
  SCRIPTS_DIR="$(cd "$(dirname "$0")" && pwd)"
fi
FAILURES=0
TESTS=0

# --- tiny assertion helpers -------------------------------------------------

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  FAILURES=$((FAILURES + 1))
}

pass() {
  printf 'ok: %s\n' "$1"
}

check() { # check <description> <condition-already-evaluated:0|1>
  TESTS=$((TESTS + 1))
  if [ "$2" -eq 0 ]; then pass "$1"; else fail "$1"; fi
}

# Build a sandbox: a fake repo root containing a copy of `scripts/<name>` plus a
# `fakebin/` we prepend to PATH. Echoes the sandbox path.
make_sandbox() {
  local name="$1"
  local sb
  sb="$(mktemp -d)"
  mkdir -p "$sb/scripts" "$sb/fakebin"
  cp "$SCRIPTS_DIR/$name" "$sb/scripts/$name"
  chmod +x "$sb/scripts/$name"
  if [[ "$name" == rue-storage ]]; then
    cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
EOF
    chmod +x "$sb/buck2"
  fi
  printf '%s\n' "$sb"
}

# Write a fake executable that logs its argv to $CALLS and runs the given body.
# Usage: fake <sandbox> <name> <body...>
fake() {
  local sb="$1" name="$2"
  shift 2
  {
    printf '#!/usr/bin/env bash\n'
    printf 'printf "%%s " "%s" >>"$CALLS"; printf "%%s\\n" "$*" >>"$CALLS"\n' "$name"
    printf '%s\n' "$*"
  } >"$sb/fakebin/$name"
  chmod +x "$sb/fakebin/$name"
}

run_script() { # run_script <sandbox> <name> [args...]
  local sb="$1" name="$2"
  shift 2
  ( cd "$sb" && CALLS="$sb/calls.log" PATH="$sb/fakebin:$PATH" \
      bash "$sb/scripts/$name" "$@" ) >"$sb/out.log" 2>&1
}

calls() { cat "$1/calls.log" 2>/dev/null; }

# ===========================================================================
# jj-tidy — remote branch deletion must be fail-closed
# ===========================================================================

# A branch is deletable ONLY when it matches a head ref of one of OUR
# merged/closed PRs, proven by a successful query. These fakes never let step 4
# (jj abandon) touch anything real: `jj` is faked to a no-op.
setup_jjtidy_common() {
  local sb="$1"
  fake "$sb" jj 'true'
  # git: only the invocations jj-tidy makes. `branch -r` lists remote push
  # branches; everything else is a harmless no-op that still gets logged.
  cat >"$sb/fakebin/git" <<'EOF'
#!/usr/bin/env bash
printf 'git ' >>"$CALLS"; printf '%s\n' "$*" >>"$CALLS"
case "$1 $2" in
  "branch -r")
    # Two remote push branches exist on origin.
    printf 'origin/steveklabnik/push-aaa\n'
    printf 'origin/otheruser/push-bbb\n'
    ;;
  "branch --format="*|"branch --format")
    : ;;  # no local worker branches
  *) : ;;
esac
exit 0
EOF
  chmod +x "$sb/fakebin/git"
}

# Test 1: gh PR-state query FAILS -> no remote deletion at all.
test_jjtidy_gh_failure_deletes_nothing() {
  local sb; sb="$(make_sandbox jj-tidy)"
  setup_jjtidy_common "$sb"
  # gh: `api user` succeeds (login), but every `pr list` fails (network/auth).
  cat >"$sb/fakebin/gh" <<'EOF'
#!/usr/bin/env bash
printf 'gh ' >>"$CALLS"; printf '%s\n' "$*" >>"$CALLS"
case "$1 $2" in
  "api user") printf 'steveklabnik\n'; exit 0 ;;
  "pr list") exit 1 ;;   # query failure
esac
exit 0
EOF
  chmod +x "$sb/fakebin/gh"

  run_script "$sb" jj-tidy
  # Herestring, not a pipe: under `pipefail` a matching `grep -q` kills the
  # producer with EPIPE and the assertion silently reads false (RUE-1155).
  if grep -q 'git push .*--delete' <<<"$(calls "$sb")"; then
    check "jj-tidy: gh failure issues NO remote deletion" 1
  else
    check "jj-tidy: gh failure issues NO remote deletion" 0
  fi
  grep -q 'fail-closed' "$sb/out.log" || fail "jj-tidy: expected fail-closed notice on gh failure"
  rm -rf "$sb"
}

# Test 2: another author's open branch (no closed/merged PR of ours) is kept;
# only OUR merged branch is deleted.
test_jjtidy_only_deletes_proven_merged() {
  local sb; sb="$(make_sandbox jj-tidy)"
  setup_jjtidy_common "$sb"
  # gh: our merged PRs include push-aaa; closed empty; open lists nothing
  # relevant. push-bbb belongs to otheruser and is NOT in our merged/closed set.
  cat >"$sb/fakebin/gh" <<'EOF'
#!/usr/bin/env bash
printf 'gh ' >>"$CALLS"; printf '%s\n' "$*" >>"$CALLS"
if [ "$1 $2" = "api user" ]; then printf 'steveklabnik\n'; exit 0; fi
if [ "$1 $2" = "pr list" ]; then
  case "$*" in
    *"--state merged"*) printf 'steveklabnik/push-aaa\n'; exit 0 ;;
    *"--state closed"*) exit 0 ;;   # none
    *"--state open"*)   exit 0 ;;
  esac
fi
exit 0
EOF
  chmod +x "$sb/fakebin/gh"

  run_script "$sb" jj-tidy
  local del
  del="$(calls "$sb" | grep 'git push .*--delete' || true)"
  # push-aaa (ours, merged) must be deleted.
  if grep -q 'steveklabnik/push-aaa' <<<"$del"; then
    check "jj-tidy: OUR merged branch is deleted" 0
  else
    check "jj-tidy: OUR merged branch is deleted" 1
  fi
  # push-bbb (other author, no merged/closed PR of ours) must NOT be deleted.
  if grep -q 'push-bbb' <<<"$del"; then
    check "jj-tidy: another author's branch is preserved" 1
  else
    check "jj-tidy: another author's branch is preserved" 0
  fi
  rm -rf "$sb"
}

# ===========================================================================
# rue-storage — Buck cleanup must be host-wide and fail-closed
# ===========================================================================

setup_storage_root() {
  local root="$1"
  mkdir -p "$root/buck-out"
  : >"$root/.buckconfig"
  cat >"$root/buck2" <<'EOF'
#!/usr/bin/env bash
printf 'unexpected worktree-local Buck invocation\n' >>"$CALLS"
exit 99
EOF
  chmod +x "$root/buck2"
}

test_storage_git_failure_is_fail_closed() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  : >"$sb/.buckconfig"
  printf '#!/bin/sh\nexit 0\n' >"$sb/buck2"; chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<'EOF'
#!/usr/bin/env bash
printf 'git ' >>"$CALLS"; printf '%s\n' "$*" >>"$CALLS"
exit 128
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage clean || rc=$?
  check "storage: failed inventory refuses cleanup" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "storage: failed inventory invokes no Buck cleaner" "$(! grep -q ':clean ' "$sb/calls.log" && echo 0 || echo 1)"
  grep -q 'fail-closed' "$sb/out.log" || fail "storage: expected fail-closed notice on git failure"
  rm -rf "$sb"
}

test_storage_plans_every_registered_root() {
  local sb; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
printf 'git ' >>"\$CALLS"; printf '%s\n' "\$*" >>"\$CALLS"
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf 'worktree %s/root-1\n\nworktree %s/root-2\n' "$sb" "$sb"
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage plan 2d
  check "storage: dry-run covers every registered Rue worktree" \
    "$([ "$(grep -c ':clean --stale 2d' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: plan never applies its cleanup" \
    "$([ "$(grep -c -- '--dry-run' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: dry-run includes the adaptive host-free target" \
    "$([ "$(grep -c -- '--adaptive-low-disk-threshold 20' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: dry-run protects the minimum TTL" \
    "$([ "$(grep -c -- '--adaptive-min-ttl 12h' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: dry-run never performs a full reset" \
    "$(! grep -Eq ':clean$' "$sb/calls.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_is_host_wide_only_under_pressure() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf 'worktree %s/root-1\n\nworktree %s/root-2\n' "$sb" "$sb"
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"

  # First probe is below the 10% emergency threshold; the post-clean probe is
  # above the 20% target.
  cat >"$sb/fakebin/df" <<EOF
#!/usr/bin/env bash
count=0
[[ -f "$sb/df-count" ]] && count=\$(cat "$sb/df-count")
printf '%d\n' \$((count + 1)) >"$sb/df-count"
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
if [[ \$count -eq 0 ]]; then
  printf 'disk 100000 95000 5000 95%% /\n'
else
  printf 'disk 100000 75000 25000 75%% /\n'
fi
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard || rc=$?
  check "storage: pressure guard succeeds after recovering headroom" "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "storage: pressure guard cleans every registered worktree" \
    "$([ "$(grep -c ':clean --stale 1w' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: pressure guard uses adaptive materializer cleanup" \
    "$([ "$(grep -c -- '--adaptive-low-disk-threshold 20 --adaptive-min-ttl 12h' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"

  : >"$sb/calls.log"
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 100000 89500 10500 89.5%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  rc=0
  run_script "$sb" rue-storage guard || rc=$?
  check "storage: healthy guard succeeds without cleanup" "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "storage: healthy guard starts no Buck cleanup" \
    "$(! grep -q ':clean ' "$sb/calls.log" 2>/dev/null && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_blocks_when_pressure_remains_critical() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf 'legacy output\n' >"$sb/root-1/buck-out/legacy"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf 'worktree %s/root-1\n' "$sb"
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 100000 95000 5000 95%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard || rc=$?
  check "storage: guard refuses a build when cleanup cannot escape critical pressure" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "storage: critical guard tries a materializer-consistent legacy reset" \
    "$(grep -q ':clean --exit-when notidle' "$sb/calls.log" && echo 0 || echo 1)"
  check "storage: coordinator never trusts a worktree-local Buck wrapper" \
    "$(! grep -q 'unexpected worktree-local' "$sb/calls.log" && echo 0 || echo 1)"
  grep -q 'stopped before risking ENOSPC\|still has only' "$sb/out.log" || \
    fail "storage: expected actionable ENOSPC prevention notice"
  rm -rf "$sb"
}

test_storage_reset_validates_all_targets_first() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf 'worktree %s/root-1\n' "$sb"
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reset "$sb/root-1" "$sb/not-registered" || rc=$?
  check "storage: reset rejects an unregistered target" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "storage: reset validates every target before deleting output" \
    "$(! grep -Eq ':clean$' "$sb/calls.log" 2>/dev/null && echo 0 || echo 1)"

  : >"$sb/calls.log"
  run_script "$sb" rue-storage reset "$sb/root-1"
  check "storage: exact registered target can be reset" \
    "$(grep -Eq ':clean$' "$sb/calls.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# --- run everything ---------------------------------------------------------

test_jjtidy_gh_failure_deletes_nothing
test_jjtidy_only_deletes_proven_merged
test_storage_git_failure_is_fail_closed
test_storage_plans_every_registered_root
test_storage_guard_is_host_wide_only_under_pressure
test_storage_guard_blocks_when_pressure_remains_critical
test_storage_reset_validates_all_targets_first

echo "--------------------------------------------------"
if [ "$FAILURES" -eq 0 ]; then
  echo "cleanup-script tests: all $TESTS checks passed"
  exit 0
else
  echo "cleanup-script tests: $FAILURES of $TESTS checks FAILED"
  exit 1
fi
