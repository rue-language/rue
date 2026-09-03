#!/usr/bin/env bash
# test-cleanup-scripts.sh — fail-closed regression tests for the maintenance
# scripts jj-tidy and rue-storage (RUE-567, RUE-1225) and for the ./buck2
# wrapper's free-space floor (RUE-1934).
#
# jj-tidy performs remote branch deletion. rue-storage only deletes rebuildable
# Buck outputs, but it still refuses to infer targets from a failed inventory.
# The wrapper refuses to start a build below an absolute free-space floor and
# never cleans anything on a build's behalf: the cross-worktree cleanup it used
# to run caused RUE-1331 and RUE-1683 and did not prevent RUE-1790.
#
# Each test runs a copy of the real script in a throwaway sandbox with fake
# tools, so no real repo, remote, or Buck output is touched.
set -uo pipefail

# Root holding the scripts under test. Under buck2 sh_test this is the
# materialized `:cleanup-script-inputs` filegroup (RUE_CLEANUP_SCRIPTS_ROOT);
# run directly from a checkout it defaults to the repository root.
if [ -n "${RUE_CLEANUP_SCRIPTS_ROOT:-}" ]; then
  ROOT_DIR="$RUE_CLEANUP_SCRIPTS_ROOT"
else
  ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
fi
SCRIPTS_DIR="$ROOT_DIR/scripts"
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
    mkdir -p "$sb/.git"
    mkdir -p "$sb/.git/worktrees/root-1" "$sb/.git/worktrees/root-2" "$sb/.git/worktrees/root-3"
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
  local root="$1" parent name admin
  mkdir -p "$root"
  parent="$(cd "$root/.." && pwd -P)"
  name="${root##*/}"
  admin="$parent/.git/worktrees/$name"
  mkdir -p "$root/buck-out"
  mkdir -p "$admin"
  printf 'gitdir: %s\n' "$admin" >"$root/.git"
  printf '%s/.git\n' "$root" >"$admin/gitdir"
  : >"$root/.buckconfig"
  cat >"$root/buck2" <<'EOF'
#!/usr/bin/env bash
printf 'unexpected worktree-local Buck invocation\n' >>"$CALLS"
exit 99
EOF
  chmod +x "$root/buck2"
}

# A fake git whose worktree inventory lists the named sandbox roots.
setup_storage_inventory() {
  local sb="$1"
  shift
  local listing="" root
  for root in "$@"; do
    listing+="worktree $sb/$root\\n\\n"
  done
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
printf 'git ' >>"\$CALLS"; printf '%s\n' "\$*" >>"\$CALLS"
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf '$listing'
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"
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
  check "storage: failed inventory invokes no Buck cleaner" "$(! grep -q ':clean' "$sb/calls.log" && echo 0 || echo 1)"
  grep -q 'fail-closed' "$sb/out.log" || fail "storage: expected fail-closed notice on git failure"
  rm -rf "$sb"
}

test_storage_plans_every_registered_root() {
  local sb; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  setup_storage_inventory "$sb" root-1 root-2
  run_script "$sb" rue-storage plan 2d
  check "storage: dry-run covers every registered Rue worktree" \
    "$([ "$(grep -c ':clean --stale 2d --tracked-only --dry-run$' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: plan is Buck's own tracked stale cleanup and nothing more" \
    "$(! grep -q -- '--adaptive' "$sb/calls.log" && ! grep -Eq ':clean$' "$sb/calls.log" && echo 0 || echo 1)"

  : >"$sb/calls.log"
  run_script "$sb" rue-storage clean 3d
  check "storage: clean applies the same cleanup to every registered root" \
    "$([ "$(grep -c ':clean --stale 3d --tracked-only$' "$sb/calls.log")" -eq 2 ] && ! grep -q -- '--dry-run' "$sb/calls.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reset_validates_all_targets_first() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_inventory "$sb" root-1
  run_script "$sb" rue-storage reset "$sb/root-1" "$sb/not-registered" || rc=$?
  check "storage: reset rejects an unregistered target" "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  check "storage: reset validates every target before deleting output" \
    "$(! grep -Eq ':clean$' "$sb/calls.log" 2>/dev/null && echo 0 || echo 1)"

  : >"$sb/calls.log"
  run_script "$sb" rue-storage reset "$sb/root-1"
  check "storage: exact registered target can be reset" \
    "$(grep -Eq "^$sb/root-1:clean$" "$sb/calls.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# ===========================================================================
# ./buck2 wrapper — the free-space floor refuses, and never cleans
# ===========================================================================

# The real wrapper and its DotSlash manifest, a fake dotslash that records the
# argv it would have run, and a fake `df -Pk` reporting FAKE_FREE_KIB free (or
# failing outright under FAKE_DF_FAIL=1).
make_wrapper_sandbox() {
  local sb
  sb="$(mktemp -d)"
  mkdir -p "$sb/fakebin" "$sb/config"
  cp "$ROOT_DIR/buck2" "$sb/buck2"
  cp "$ROOT_DIR/buck2-bin" "$sb/buck2-bin"
  chmod +x "$sb/buck2"
  # The first argument is the DotSlash manifest; the log holds Buck's argv.
  cat >"$sb/fakebin/dotslash" <<'EOF'
#!/usr/bin/env bash
shift
printf '%s\n' "$@" >"$DOTSLASH_ARGS"
EOF
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
if [ "${FAKE_DF_FAIL:-0}" = 1 ]; then exit 1; fi
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf '/dev/fake 100000000 1000 %s 1%% /\n' "$FAKE_FREE_KIB"
EOF
  chmod +x "$sb/fakebin/dotslash" "$sb/fakebin/df"
  printf '%s\n' "$sb"
}

# run_wrapper <sandbox> <free-kib> <argv-log> [buck2 args...]. The config
# lookups point into the empty sandbox so no installed cache config on the
# host can reach the argv under test.
run_wrapper() {
  local sb="$1" free="$2" log="$3"
  shift 3
  ( cd "$sb" && FAKE_FREE_KIB="$free" DOTSLASH_ARGS="$sb/$log" \
      HOME="$sb/home" XDG_CONFIG_HOME="$sb/config" RUE_BUILDBUDDY_CONFIG="$sb/absent-config" \
      PATH="$sb/fakebin:$PATH" ./buck2 "$@" ) >"$sb/out.log" 2>&1
}

test_wrapper_refuses_below_free_space_floor() {
  local sb rc gib=1048576; sb="$(make_wrapper_sandbox)"

  rc=0; run_wrapper "$sb" $((1 * gib)) refused.args build //:probe || rc=$?
  check "wrapper: a build below the 4 GiB floor is refused before Buck runs" \
    "$([ "$rc" -ne 0 ] && [ ! -e "$sb/refused.args" ] && echo 0 || echo 1)"
  check "wrapper: the refusal names worktree removal, storage reset, and storage clean" \
    "$(grep -q 'git worktree remove' "$sb/out.log" && grep -q 'storage reset' "$sb/out.log" && grep -q 'storage clean' "$sb/out.log" && echo 0 || echo 1)"

  rc=0; run_wrapper "$sb" $((1 * gib)) clean.args clean || rc=$?
  check "wrapper: clean stays available below the floor" \
    "$([ "$rc" -eq 0 ] && [ "$(head -n 1 "$sb/clean.args")" = clean ] && echo 0 || echo 1)"

  rc=0; run_wrapper "$sb" $((1 * gib)) targets.args targets //:probe || rc=$?
  check "wrapper: non-execution commands are never refused" \
    "$([ "$rc" -eq 0 ] && [ -e "$sb/targets.args" ] && echo 0 || echo 1)"

  rc=0; run_wrapper "$sb" $((4 * gib)) floor.args test //:probe || rc=$?
  check "wrapper: exactly 4 GiB free passes the floor" \
    "$([ "$rc" -eq 0 ] && [ "$(head -n 1 "$sb/floor.args")" = test ] && echo 0 || echo 1)"

  rc=0; run_wrapper "$sb" $((8 * gib)) above.args build //:probe || rc=$?
  check "wrapper: above the floor the build runs with its argv unchanged" \
    "$([ "$rc" -eq 0 ] && [ "$(tr '\n' ' ' <"$sb/above.args")" = 'build //:probe ' ] && echo 0 || echo 1)"

  rc=0; FAKE_DF_FAIL=1 run_wrapper "$sb" 0 nodf.args build //:probe || rc=$?
  check "wrapper: an unmeasurable filesystem fails closed" \
    "$([ "$rc" -ne 0 ] && [ ! -e "$sb/nodf.args" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

# --- run everything ---------------------------------------------------------

test_jjtidy_gh_failure_deletes_nothing
test_jjtidy_only_deletes_proven_merged
test_storage_git_failure_is_fail_closed
test_storage_plans_every_registered_root
test_storage_reset_validates_all_targets_first
test_wrapper_refuses_below_free_space_floor

echo "--------------------------------------------------"
if [ "$FAILURES" -eq 0 ]; then
  echo "cleanup-script tests: all $TESTS checks passed"
  exit 0
else
  echo "cleanup-script tests: $FAILURES of $TESTS checks FAILED"
  exit 1
fi
