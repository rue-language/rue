#!/usr/bin/env bash
# test-cleanup-scripts.sh — fail-closed regression tests for the destructive
# maintenance scripts jj-tidy and worktree-gc (RUE-567).
#
# Both scripts perform irreversible operations (remote branch deletion,
# `rm -rf`) driven by Git/GitHub inventory. The bug these tests pin: treating a
# FAILED or incomplete inventory query as proof that a resource is stale, so a
# `gh`/`git` failure — or a branch merely absent from one user's open-PR list —
# caused live resources to be destroyed.
#
# Each test runs a COPY of the real script in a throwaway sandbox with fake
# `gh`/`git`/`jj`/`df`/`buck2` on PATH, so no real repo, remote, or disk is
# touched. The fakes log every invocation; we assert that destructive commands
# (`git push --delete`, `rm -rf`) are or are not issued as the fail-closed
# contract requires.
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
# worktree-gc — worktree removal must be fail-closed
# ===========================================================================

# Common fakes: df reports high usage (past threshold) so step 2 runs; buck2 and
# jj no-op. Real `rm`/`git` behavior is per-test.
setup_gc_common() {
  local sb="$1"
  fake "$sb" df 'echo "Use% "; echo "99%"'
  fake "$sb" buck2 'true'
  mkdir -p "$sb/.claude/worktrees/live-1" "$sb/.claude/worktrees/live-2"
}

# Test 3: `git worktree list` FAILS (exit 128) -> no worktree dir removed.
test_gc_git_failure_preserves_worktrees() {
  local sb; sb="$(make_sandbox worktree-gc)"
  setup_gc_common "$sb"
  cat >"$sb/fakebin/git" <<'EOF'
#!/usr/bin/env bash
printf 'git ' >>"$CALLS"; printf '%s\n' "$*" >>"$CALLS"
case "$1 $2" in
  "worktree prune") exit 0 ;;
  "worktree list") exit 128 ;;   # jj workspace w/o local .git
esac
exit 0
EOF
  chmod +x "$sb/fakebin/git"

  run_script "$sb" worktree-gc
  if [ -d "$sb/.claude/worktrees/live-1" ] && [ -d "$sb/.claude/worktrees/live-2" ]; then
    check "worktree-gc: git-list failure preserves ALL worktrees" 0
  else
    check "worktree-gc: git-list failure preserves ALL worktrees" 1
  fi
  grep -q 'fail-closed' "$sb/out.log" || fail "worktree-gc: expected fail-closed notice on git failure"
  rm -rf "$sb"
}

# Test 4 (positive control): git succeeds and lists live-1 as active; live-2 is
# absent -> only the genuinely-orphaned live-2 is removed.
test_gc_removes_only_orphans_when_git_ok() {
  local sb; sb="$(make_sandbox worktree-gc)"
  setup_gc_common "$sb"
  # Emit live-1's absolute path as an active worktree; omit live-2.
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
printf 'git ' >>"\$CALLS"; printf '%s\n' "\$*" >>"\$CALLS"
case "\$1 \$2" in
  "worktree prune") exit 0 ;;
  "worktree list") printf 'worktree %s/.claude/worktrees/live-1\n' "$sb"; exit 0 ;;
esac
exit 0
EOF
  chmod +x "$sb/fakebin/git"

  run_script "$sb" worktree-gc
  if [ -d "$sb/.claude/worktrees/live-1" ]; then
    check "worktree-gc: active worktree kept when git succeeds" 0
  else
    check "worktree-gc: active worktree kept when git succeeds" 1
  fi
  if [ -d "$sb/.claude/worktrees/live-2" ]; then
    check "worktree-gc: genuinely-orphaned worktree removed" 1
  else
    check "worktree-gc: genuinely-orphaned worktree removed" 0
  fi
  rm -rf "$sb"
}

# Test 5: disk below threshold -> step 2 never runs, nothing removed.
test_gc_below_threshold_is_noop() {
  local sb; sb="$(make_sandbox worktree-gc)"
  fake "$sb" df 'echo "Use% "; echo "10%"'
  fake "$sb" buck2 'true'
  fake "$sb" git 'true'
  mkdir -p "$sb/.claude/worktrees/live-1"
  run_script "$sb" worktree-gc
  if [ -d "$sb/.claude/worktrees/live-1" ]; then
    check "worktree-gc: below threshold removes nothing" 0
  else
    check "worktree-gc: below threshold removes nothing" 1
  fi
  rm -rf "$sb"
}

# --- run everything ---------------------------------------------------------

test_jjtidy_gh_failure_deletes_nothing
test_jjtidy_only_deletes_proven_merged
test_gc_git_failure_preserves_worktrees
test_gc_removes_only_orphans_when_git_ok
test_gc_below_threshold_is_noop

echo "--------------------------------------------------"
if [ "$FAILURES" -eq 0 ]; then
  echo "cleanup-script tests: all $TESTS checks passed"
  exit 0
else
  echo "cleanup-script tests: $FAILURES of $TESTS checks FAILED"
  exit 1
fi
