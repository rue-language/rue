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

# reclaim-finished now scopes even Buck's default v2 isolation explicitly;
# recognize both that argv shape and the legacy host-wide `clean` form. Keep
# reset/TTL assertions on their original matchers because those paths retain
# their distinct contracts.
has_reclaim_buck_destructive_call() {
  grep -Eq '(^|:)((--isolation-dir [^[:space:]]+ )?clean) --exit-when notidle([[:space:]]|$)' "$1/calls.log" 2>/dev/null
}

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
  check "storage: dry-run considers tracked artifacts only" \
    "$([ "$(grep -c -- '--tracked-only' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
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
  check "storage: pressure guard considers tracked artifacts only" \
    "$([ "$(grep -c -- '--tracked-only' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"

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

test_storage_guard_preserves_incremental_rustc_scaffolding() {
  # RUE-1683: adaptive stale cleanup must not remove untracked rustc out-dir
  # scaffolding from a live sibling worktree. The fake Buck coordinator models
  # the old behavior by deleting that directory unless --tracked-only is set,
  # then models a subsequent incremental build that requires it.
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  mkdir -p "$sb/root-2/buck-out/v2/gen/extras/rue-codegen"

  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
case "$1" in
  clean)
    if [[ "$*" == *"--stale"* && "$*" != *"--tracked-only"* ]]; then
      rmdir "$PWD/buck-out/v2/gen/extras/rue-codegen" 2>/dev/null || true
    fi
    ;;
  build)
    [[ -d "$PWD/buck-out/v2/gen/extras/rue-codegen" ]]
    ;;
esac
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf 'worktree %s/root-1\n\nworktree %s/root-2\n' "$sb" "$sb"
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"
  # Start below the emergency threshold, then recover above the adaptive
  # target after cleanup. No host-wide disk state is touched.
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
  check "storage: guard preserves a sibling's rustc out-dir scaffolding" \
    "$([ -d "$sb/root-2/buck-out/v2/gen/extras/rue-codegen" ] && echo 0 || echo 1)"
  (cd "$sb/root-2" && CALLS="$sb/calls.log" PATH="$sb/fakebin:$PATH" \
    "$sb/buck2" build --incremental) || rc=1
  check "storage: subsequent sibling incremental build remains viable" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
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

test_storage_guard_proceeds_between_cleanup_and_hard_floors() {
  # RUE-1331: several active worktrees legitimately pin a large host between
  # the 16 GiB cleanup floor and the 4 GiB hard floor for hours. Cleanup finds
  # nothing reclaimable there (the artifacts are live), and a refusal starves
  # every build without protecting the host — the guard must warn and proceed.
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
  # 15 GiB free of 200 GiB: 7.5%, below the cleanup floor, above the hard
  # floor. Constant across probes — cleanup reclaims nothing, as on a host
  # whose space is held by active sibling builds.
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 209715200 193986560 15728640 93%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard || rc=$?
  check "storage: guard proceeds between the cleanup and hard floors" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  check "storage: between-floors guard still attempts cleanup first" \
    "$(grep -q ':clean --stale 1w' "$sb/calls.log" && echo 0 || echo 1)"
  grep -q 'above the 4 GiB hard floor; proceeding' "$sb/out.log" || \
    fail "storage: expected the warn-and-proceed notice between floors"
  grep -q 'Do not edit or bypass this guard' "$sb/out.log" || \
    fail "storage: expected the sanctioned-remediation notice"
  rm -rf "$sb"
}

test_storage_guard_survives_cleanup_failure_between_floors() {
  # RUE-1331 follow-up: sibling worktrees mid-build make their Buck daemons
  # refuse `clean`. A cleanup ERROR must not be fatal in the middle band —
  # the hard floor decides. Observed live: three cycle-5 workers building
  # blocked the coordinator's build through this exact path.
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
exit 1
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"worktree list --porcelain"* ]]; then
  printf 'worktree %s/root-1\n' "$sb"
  exit 0
fi
exit 1
EOF
  chmod +x "$sb/fakebin/git"
  # 15 GiB free of 200 GiB: middle band, constant across probes.
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 209715200 193986560 15728640 93%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard || rc=$?
  check "storage: cleanup failure between floors still proceeds" \
    "$([ "$rc" -eq 0 ] && echo 0 || echo 1)"
  grep -q 'deciding on the hard floor instead' "$sb/out.log" || \
    fail "storage: expected the cleanup-failure demotion notice"
  rm -rf "$sb"
}

test_storage_guard_still_refuses_below_hard_floor_when_cleanup_fails() {
  # The safety property survives the demotion: a cleanup error below the hard
  # floor is still a refusal.
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
exit 1
EOF
  chmod +x "$sb/buck2"
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
  check "storage: cleanup failure below the hard floor still refuses" \
    "$([ "$rc" -ne 0 ] && echo 0 || echo 1)"
  grep -q 'hard floor; build stopped before risking ENOSPC' "$sb/out.log" || \
    fail "storage: expected the hard-floor refusal notice"
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

test_storage_reclaim_finished_preserves_dirty_and_clean_sources() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  setup_storage_root "$sb/root-3"
  printf '../../../root-1/.git\n' >"$sb/.git/worktrees/root-1/gitdir"
  printf 'dirty source\n' >"$sb/root-1/dirty.rue"
  printf 'clean source\n' >"$sb/root-2/clean.rue"
  printf 'output\n' >"$sb/root-1/buck-out/artifact"
  printf 'output\n' >"$sb/root-2/buck-out/artifact"
  printf 'sibling output\n' >"$sb/root-3/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then rm -rf "$PWD/buck-out"; fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n\\nworktree %s/root-2\\n\\nworktree %s/root-3\\n' "$sb" "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *root-2*) printf '%s/.git/worktrees/root-2\\n' "$sb" ;; *root-3*) printf '%s/.git/worktrees/root-3\\n' "$sb" ;; *) printf '%s/.git\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) if [[ "\$*" == *root-1* ]]; then printf ' M dirty.rue\\n'; fi ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" "$sb/root-2" || rc=$?
  check "storage: reclaim-finished covers dirty and clean roots" "$([ "$rc" -eq 0 ] && [ "$(grep -c 'clean --exit-when notidle$' "$sb/calls.log")" -eq 2 ] && echo 0 || echo 1)"
  check "storage: dirty source survives reclaim byte-for-byte" "$([ -f "$sb/root-1/dirty.rue" ] && [ "$(cat "$sb/root-1/dirty.rue")" = 'dirty source' ] && echo 0 || echo 1)"
  check "storage: clean source survives reclaim byte-for-byte" "$([ -f "$sb/root-2/clean.rue" ] && [ "$(cat "$sb/root-2/clean.rue")" = 'clean source' ] && echo 0 || echo 1)"
  check "storage: unnamed sibling output survives reclaim" "$([ -f "$sb/root-3/buck-out/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_invalid_and_current_targets() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  mkdir -p "$sb/counterfeit/buck-out"
  : >"$sb/counterfeit/.buckconfig"
  printf '#!/bin/sh\n' >"$sb/counterfeit/buck2"; chmod +x "$sb/counterfeit/buck2"
  : >"$sb/.buckconfig"
  ln -s "$sb" "$sb/current-alias"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s\\n\\nworktree %s/root-1\\n' "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *) printf '%s/.git\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" "$sb/counterfeit" || rc=$?
  check "storage: counterfeit target is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rc=0; : >"$sb/calls.log"
  run_script "$sb" rue-storage reclaim-finished "$sb" || rc=$?
  check "storage: current coordinator root is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rc=0; : >"$sb/calls.log"
  run_script "$sb" rue-storage reclaim-finished "$sb/current-alias" || rc=$?
  check "storage: current coordinator symlink alias is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_replacement_clone_identity() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  mkdir -p "$sb/other.git"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) if [[ "\$*" == *root-1* ]]; then printf '%s/other.git\\n' "$sb"; else printf '%s/.git\\n' "$sb"; fi ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: replacement clone at registered path is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_ignores_ambient_git_selection() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  mkdir -p "$sb/foreign.git" "$sb/foreign-root"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ -n "\${GIT_DIR:-}" || -n "\${GIT_WORK_TREE:-}" || -n "\${GIT_COMMON_DIR:-}" || -n "\${GIT_INDEX_FILE:-}" ]]; then
  case "\$*" in
    *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
    *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
    *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
    *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
    *"status --porcelain"*) : ;;
    *) exit 1 ;;
  esac
else
  case "\$*" in
    *"worktree list --porcelain"*) printf 'worktree %s\\n' "$sb" ;;
    *) exit 1 ;;
  esac
fi
EOF
  chmod +x "$sb/fakebin/git"
  GIT_DIR="$sb/foreign.git" GIT_WORK_TREE="$sb/foreign-root" GIT_COMMON_DIR="$sb/foreign.git" GIT_INDEX_FILE="$sb/foreign-index" \
    run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: ambient Git selection cannot authorize a foreign root" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && [ -f "$sb/root-1/buck-out/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_main_metadata_counterfeit() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  rm -f "$sb/root-1/.git"
  ln -s "$sb/.git" "$sb/root-1/.git"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: main-worktree metadata counterfeit is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && [ -f "$sb/root-1/buck-out/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_same_common_counterfeit() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  mkdir -p "$sb/.git/other-admin"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/other-admin\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: same-common-dir counterfeit with wrong worktree admin is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && [ -f "$sb/root-1/buck-out/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_isolation_symlink_escape() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/outside"
  printf outside >"$sb/outside/artifact"
  ln -s "$sb/outside" "$sb/root-1/buck-out/escape"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: symlinked direct isolation is refused without Buck cleanup" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && [ -f "$sb/outside/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_non_directory_output_root() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  rm -rf "$sb/root-1/buck-out"
  printf 'not a directory' >"$sb/root-1/buck-out"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: non-directory Buck output root is refused" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && [ "$(cat "$sb/root-1/buck-out")" = 'not a directory' ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_output_root_swap() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/outside"
  printf output >"$sb/root-1/buck-out/artifact"
  printf outside >"$sb/outside/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *--dry-run* ]]; then
  mv "$PWD/buck-out" "$PWD/buck-out-saved"
  ln -s "$PWD/../outside" "$PWD/buck-out"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  : >"$CALLS.destructive"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: swapped Buck output root is refused before destructive clean" "$([ "$rc" -ne 0 ] && [ ! -e "$sb/calls.log.destructive" ] && [ -f "$sb/root-1/buck-out-saved/artifact" ] && [ "$(cat "$sb/outside/artifact")" = outside ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_regular_output_root_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf original >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *--dry-run* ]]; then
  mv "$PWD/buck-out" "$PWD/buck-out-saved"
  mkdir -p "$PWD/buck-out/v2"
  printf replacement >"$PWD/buck-out/v2/artifact"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  : >"$CALLS.destructive"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: same-path output-root generation replacement is refused" "$([ "$rc" -ne 0 ] && grep -q 'output root identity changed' "$sb/out.log" && [ ! -e "$sb/calls.log.destructive" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && [ -f "$sb/root-1/buck-out-saved/v2/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_initial_probe_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf original >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"status --porcelain"* && ! -e "\$CALLS.probed" ]]; then
  : >"\$CALLS.probed"
  mv "$sb/root-1" "$sb/root-1-probed"
  mkdir "$sb/root-1"
  cp "$sb/root-1-probed/.git" "$sb/root-1/.git"
  cp "$sb/root-1-probed/.buckconfig" "$sb/root-1/.buckconfig"
  cp "$sb/root-1-probed/buck2" "$sb/root-1/buck2"
  mv "$sb/root-1-probed/buck-out" "$sb/root-1/buck-out"
  printf '%s/.git\\n' "$sb/root-1" >"$sb/.git/worktrees/root-1/gitdir"
fi
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: initial probe root replacement is refused" "$([ "$rc" -ne 0 ] && [ -e "$sb/calls.log.probed" ] && [ -d "$sb/root-1-probed" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && grep -q 'registered worktree identity changed during initial reclaim validation' "$sb/out.log" && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_initial_probe_isolation_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf original >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"status --porcelain"* && ! -e "\$CALLS.child-probed" ]]; then
  : >"\$CALLS.child-probed"
  mv "$sb/root-1/buck-out/v2" "$sb/root-1/v2-probed"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf replacement >"$sb/root-1/buck-out/v2/artifact"
fi
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: initial probe isolation replacement is refused" "$([ "$rc" -ne 0 ] && [ -e "$sb/calls.log.child-probed" ] && [ -f "$sb/root-1/v2-probed/artifact" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && grep -q 'Buck isolation generation changed during initial reclaim validation' "$sb/out.log" && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_initial_probe_new_isolation() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf original >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
if [[ "\$*" == *"status --porcelain"* && ! -e "\$CALLS.new-isolation" ]]; then
  : >"\$CALLS.new-isolation"
  mkdir -p "$sb/root-1/buck-out/compiler-repro"
  printf new >"$sb/root-1/buck-out/compiler-repro/artifact"
fi
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: initial probe new isolation is refused" "$([ "$rc" -ne 0 ] && [ -e "$sb/calls.log.new-isolation" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && [ -f "$sb/root-1/buck-out/compiler-repro/artifact" ] && grep -q 'Buck isolation generation changed during initial reclaim validation' "$sb/out.log" && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_regular_isolation_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf original >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *--dry-run* ]]; then
  mv "$PWD/buck-out/v2" "$PWD/v2-saved"
  mkdir -p "$PWD/buck-out/v2"
  printf replacement >"$PWD/buck-out/v2/artifact"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  : >"$CALLS.destructive"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: same-path isolation generation replacement is refused" "$([ "$rc" -ne 0 ] && grep -q 'isolation identity changed' "$sb/out.log" && [ ! -e "$sb/calls.log.destructive" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && [ -f "$sb/root-1/v2-saved/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_worktree_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *--dry-run* ]]; then
  mv "$PWD" "$PWD-replaced"
  mkdir "$PWD"
  cp "$PWD-replaced/.buckconfig" "$PWD/.buckconfig"
  cp "$PWD-replaced/buck2" "$PWD/buck2"
  mkdir "$PWD/buck-out"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  : >"$CALLS.destructive"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: replaced registered worktree is refused before destructive clean" "$([ "$rc" -ne 0 ] && [ ! -e "$sb/calls.log.destructive" ] && [ -f "$sb/root-1-replaced/buck-out/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_valid_same_path_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf original >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
set -e
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *--dry-run* ]]; then
  test_sb="${CALLS%/calls.log}"
  mv "$PWD" "$PWD-replaced"
  mkdir -p "$PWD/buck-out/v2" "$test_sb/.git/worktrees/replacement"
  : >"$PWD/.buckconfig"
  cp "$PWD-replaced/buck2" "$PWD/buck2"
  printf 'gitdir: %s/.git/worktrees/replacement\n' "$test_sb" >"$PWD/.git"
  printf '%s/.git\n' "$PWD" >"$test_sb/.git/worktrees/replacement/gitdir"
  printf replacement >"$PWD/buck-out/v2/artifact"
  : >"$CALLS.replaced"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  : >"$CALLS.destructive"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) if [[ -f "\$CALLS.replaced" ]]; then printf '%s/.git/worktrees/replacement\\n' "$sb"; else printf '%s/.git/worktrees/root-1\\n' "$sb"; fi ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: valid same-path Git worktree replacement is refused before destructive clean" "$([ "$rc" -ne 0 ] && [ ! -e "$sb/calls.log.destructive" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_handles_empty_isolation_that_gains_output() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf output >"$sb/root-2/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$PWD" == *'/root-1' && "$*" == *--dry-run* ]]; then
  printf gained >"$PWD/buck-out/v2/artifact"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  rm -rf "$PWD/buck-out/v2" "$PWD/buck-out/artifact"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n\\nworktree %s/root-2\\n' "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *root-2*) printf '%s/.git/worktrees/root-2\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" "$sb/root-2" || rc=$?
  check "storage: empty isolation is preflighted and reclaimed after gaining output" "$([ "$rc" -eq 0 ] && grep -q 'clean --exit-when notidle$' "$sb/calls.log" && [ ! -e "$sb/root-1/buck-out/v2" ] && [ ! -e "$sb/root-2/buck-out/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_cleans_zero_byte_output_entry() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  : >"$sb/root-1/buck-out/v2/zero-byte"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  rm -f "$PWD/buck-out/v2/zero-byte"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: zero-byte output entry is preflighted and reclaimed" "$([ "$rc" -eq 0 ] && [ ! -e "$sb/root-1/buck-out/v2/zero-byte" ] && [ "$(grep -c -- '--isolation-dir v2 clean --dry-run --exit-when notidle' "$sb/calls.log")" -eq 1 ] && [ "$(grep -c -- '--isolation-dir v2 clean --exit-when notidle' "$sb/calls.log")" -eq 1 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rechecks_empty_root_after_later_preflight() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  printf root2 >"$sb/root-2/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$PWD" == *'/root-2' && "$*" == *--dry-run* ]]; then
  mkdir -p "$PWD/../root-1/buck-out/v2"
  printf appeared >"$PWD/../root-1/buck-out/v2/artifact"
fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  : >"$CALLS.destructive"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n\\nworktree %s/root-2\\n' "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *root-2*) printf '%s/.git/worktrees/root-2\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" "$sb/root-2" || rc=$?
  check "storage: later preflight growth of an empty root is refused before all destructive cleanup" "$([ "$rc" -ne 0 ] && [ ! -e "$sb/calls.log.destructive" ] && [ -f "$sb/root-1/buck-out/v2/artifact" ] && ! grep -q 'zero Buck output' "$sb/out.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_new_isolation_after_clean() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf output >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  rm -rf "$PWD/buck-out/v2"
  mkdir -p "$PWD/buck-out/new-isolation"
  printf new >"$PWD/buck-out/new-isolation/artifact"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: new isolation after clean is refused and remains for review" "$([ "$rc" -ne 0 ] && [ -f "$sb/root-1/buck-out/new-isolation/artifact" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_reports_buck_owned_residual() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf output >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  rm -f "$PWD/buck-out/v2/artifact"
  printf buck-log >"$PWD/buck-out/v2/log"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: Buck-owned residual is reported after successful exact-isolation clean" "$([ "$rc" -eq 0 ] && grep -q 'residual' "$sb/out.log" && [ -f "$sb/root-1/buck-out/v2/log" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_reports_non_directory_residual() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf output >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  rm -f "$PWD/buck-out/v2/artifact"
  mkfifo "$PWD/buck-out/v2/buck-fifo"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: non-directory Buck residual is reported" "$([ "$rc" -eq 0 ] && grep -q 'buck-fifo' "$sb/out.log" && [ -p "$sb/root-1/buck-out/v2/buck-fifo" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_rejects_root_replacement_after_clean() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2"
  printf output >"$sb/root-1/buck-out/v2/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  mv "$PWD" "$PWD-replaced"
  mkdir -p "$PWD/buck-out/v2"
  : >"$PWD/.buckconfig"
  cp "$PWD-replaced/buck2" "$PWD/buck2"
  printf replacement >"$PWD/buck-out/v2/artifact"
  : >"$CALLS.replaced"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) if [[ -f "\$CALLS.replaced" ]]; then printf '%s/.git/worktrees/replacement\\n' "$sb"; else printf '%s/.git/worktrees/root-1\\n' "$sb"; fi ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: root replacement after clean suppresses terminal success" "$([ "$rc" -ne 0 ] && ! grep -q 'reclaimed' "$sb/out.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_explicitly_scopes_v2_with_environment_override() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2" "$sb/root-1/buck-out/compiler-repro"
  printf v2 >"$sb/root-1/buck-out/v2/artifact"
  printf repro >"$sb/root-1/buck-out/compiler-repro/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ -f "$CALLS.active-v2" && "$*" == *'--isolation-dir v2 clean'* && "$*" == *--dry-run* ]]; then exit 7; fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  if [[ "$*" == *'--isolation-dir compiler-repro'* ]]; then rm -rf "$PWD/buck-out/compiler-repro"; else rm -rf "$PWD/buck-out/v2"; fi
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  : >"$sb/calls.log.active-v2"
  BUCK_ISOLATION_DIR=compiler-repro run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: active explicit v2 isolation blocks every cleanup" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && [ -f "$sb/root-1/buck-out/v2/artifact" ] && [ -f "$sb/root-1/buck-out/compiler-repro/artifact" ] && echo 0 || echo 1)"
  rm -f "$sb/calls.log.active-v2"; : >"$sb/calls.log"; rc=0
  BUCK_ISOLATION_DIR=compiler-repro run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: environment override cannot retarget explicit isolation cleanup" "$([ "$rc" -eq 0 ] && [ "$(grep -c -- '--isolation-dir v2 clean --dry-run --exit-when notidle' "$sb/calls.log")" -eq 1 ] && [ "$(grep -c -- '--isolation-dir v2 clean --exit-when notidle' "$sb/calls.log")" -eq 1 ] && [ "$(grep -c -- '--isolation-dir compiler-repro clean --dry-run --exit-when notidle' "$sb/calls.log")" -eq 1 ] && [ "$(grep -c -- '--isolation-dir compiler-repro clean --exit-when notidle' "$sb/calls.log")" -eq 1 ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_alias_active_and_zero_output() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  printf output >"$sb/root-1/buck-out/artifact"
  ln -s "$sb/root-1" "$sb/root-1-alias"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$PWD" == *'/root-1' ]]; then exit 7; fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n\\nworktree %s/root-2\\n' "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *root-2*) printf '%s/.git/worktrees/root-2\\n' "$sb" ;; *) printf '%s/.git\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1-alias" || rc=$?
  check "storage: active Buck root is refused" "$([ "$rc" -ne 0 ] && grep -q 'root-1' "$sb/out.log" && echo 0 || echo 1)"
  : >"$sb/calls.log"; rc=0
  run_script "$sb" rue-storage reclaim-finished "$sb/root-2" || rc=$?
  check "storage: zero-output root succeeds without destructive Buck cleanup" "$([ "$rc" -eq 0 ] && ! has_reclaim_buck_destructive_call "$sb" && grep -q 'zero Buck output' "$sb/out.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_alias_and_preflight_validate_all() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  printf one >"$sb/root-1/buck-out/artifact"
  printf two >"$sb/root-2/buck-out/artifact"
  ln -s "$sb/root-1" "$sb/root-1-alias"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$PWD" == *'/root-2' && "$*" == *--dry-run* ]]; then exit 7; fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then rm -rf "$PWD/buck-out"; fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n\\nworktree %s/root-2\\n' "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *root-2*) printf '%s/.git/worktrees/root-2\\n' "$sb" ;; *) printf '%s/.git\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1-alias" "$sb/root-2" || rc=$?
  check "storage: later active preflight prevents all destructive cleanup" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  check "storage: earlier output survives later preflight refusal" "$([ -f "$sb/root-1/buck-out/artifact" ] && echo 0 || echo 1)"
  check "storage: later output survives its preflight refusal" "$([ -f "$sb/root-2/buck-out/artifact" ] && echo 0 || echo 1)"

  : >"$sb/calls.log"; rc=0
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1-alias" || rc=$?
  check "storage: symlink alias reclaims its canonical registered root" "$([ "$rc" -eq 0 ] && [ ! -e "$sb/root-1/buck-out" ] && [ "$(grep -c 'clean --exit-when notidle$' "$sb/calls.log")" -eq 1 ] && echo 0 || echo 1)"

  mkdir -p "$sb/root-1/buck-out"; printf one >"$sb/root-1/buck-out/artifact"
  : >"$sb/calls.log"; rc=0
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" "$sb/root-1-alias" || rc=$?
  check "storage: canonical and alias duplicate is refused before cleanup" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_reclaim_finished_scopes_every_isolation() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  mkdir -p "$sb/root-1/buck-out/v2" "$sb/root-1/buck-out/compiler-repro"
  printf default >"$sb/root-1/buck-out/v2/artifact"
  printf alternate >"$sb/root-1/buck-out/compiler-repro/artifact"
cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ -f "$CALLS.active-alternate" && "$*" == *--isolation-dir*compiler-repro* && "$*" == *--dry-run* ]]; then exit 7; fi
if [[ -f "$CALLS.activate-on-destructive" && "$*" == *--isolation-dir*compiler-repro* && "$*" != *--dry-run* ]]; then exit 7; fi
if [[ "$*" == *clean* && "$*" != *--dry-run* ]]; then
  if [[ "$*" == *'--isolation-dir compiler-repro'* ]]; then rm -rf "$PWD/buck-out/compiler-repro"; else rm -rf "$PWD/buck-out/v2"; fi
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$sb/fakebin/git"
: >"$sb/calls.log.active-alternate"
run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: active alternate isolation prevents all cleanup" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  check "storage: default isolation survives alternate refusal" "$([ -f "$sb/root-1/buck-out/v2/artifact" ] && echo 0 || echo 1)"
  check "storage: alternate isolation survives its refusal" "$([ -f "$sb/root-1/buck-out/compiler-repro/artifact" ] && echo 0 || echo 1)"

  rm -f "$sb/calls.log.active-alternate"
  : >"$sb/calls.log.activate-on-destructive"
  : >"$sb/calls.log"; rc=0
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: destructive alternate refusal stops within-root cleanup" "$([ "$rc" -ne 0 ] && [ ! -e "$sb/root-1/buck-out/v2" ] && [ -f "$sb/root-1/buck-out/compiler-repro/artifact" ] && echo 0 || echo 1)"
  rm -f "$sb/calls.log.activate-on-destructive"
  : >"$sb/calls.log"; rc=0
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: idle alternate isolation can reclaim after refusal" "$([ "$rc" -eq 0 ] && [ ! -e "$sb/root-1/buck-out/compiler-repro" ] && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_finished_evidence_rechecks_output_after_ttl_cleanup() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$1" == audit ]]; then printf '{"buck2.defer_write_actions":"true"}\n'; fi
if [[ "$1" == clean && "$*" == *--stale* && "$*" != *--dry-run* ]]; then rm -rf "$PWD/buck-out"; fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 100000 95000 5000 95%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard --finished-root "$sb/root-1" || rc=$?
  check "storage: guard does not advertise output removed by ordinary TTL cleanup" "$(! grep -q 'reclaim-finished' "$sb/out.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_finished_evidence_rejects_root_replacement() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$1" == audit ]]; then printf '{"buck2.defer_write_actions":"true"}\n'; fi
if [[ "$*" == *--stale* && "$*" == *--dry-run* ]]; then
  mv "$PWD" "$PWD-replaced"
  mkdir -p "$PWD"
  cp "$PWD-replaced/.buckconfig" "$PWD/.buckconfig"
  cp "$PWD-replaced/buck2" "$PWD/buck2"
  cp "$PWD-replaced/.git" "$PWD/.git"
  mkdir -p "$PWD/buck-out"
  printf replacement >"$PWD/buck-out/artifact"
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 100000 95000 5000 95%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard --finished-root "$sb/root-1" || rc=$?
  check "storage: guard rejects finished evidence after root replacement" "$([ "$rc" -ne 0 ] && [ -d "$sb/root-1-replaced" ] && [ "$(cat "$sb/root-1/buck-out/artifact")" = replacement ] && ! grep -q 'eligible for reclaim-finished' "$sb/out.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_rejects_malformed_evidence_and_reclaim_rejects_unknown_source() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  : >"$sb/.buckconfig"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s\\n' "$sb" ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  run_script "$sb" rue-storage guard --finished-root || rc=$?
  check "storage: malformed finished-root option fails with usage" "$([ "$rc" -eq 2 ] && grep -q '^usage:' "$sb/out.log" && echo 0 || echo 1)"
  check "storage: malformed finished-root option invokes no cleanup" "$(! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"

  setup_storage_root "$sb/root-1"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) exit 1 ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"; : >"$sb/calls.log"; rc=0
  run_script "$sb" rue-storage reclaim-finished "$sb/root-1" || rc=$?
  check "storage: unknown source state refuses reclaim" "$([ "$rc" -ne 0 ] && ! has_reclaim_buck_destructive_call "$sb" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_names_only_caller_supplied_finished_evidence() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  printf output >"$sb/root-1/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$1" == audit ]]; then
  printf '{"buck2.defer_write_actions":"true"}\n'
fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n' "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 100000 95000 5000 95%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard --finished-root "$sb/root-1" || rc=$?
  check "storage: guard accepts caller-supplied finished evidence" "$([ "$rc" -ne 0 ] && grep -q 'reclaim-finished' "$sb/out.log" && echo 0 || echo 1)"
  check "storage: guard evidence does not auto-reclaim" "$(! grep -q 'clean --exit-when notidle$' "$sb/calls.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

test_storage_guard_skips_active_evidence_and_names_idle_evidence() {
  local sb rc=0; sb="$(make_sandbox rue-storage)"
  setup_storage_root "$sb/root-1"
  setup_storage_root "$sb/root-2"
  printf active >"$sb/root-1/buck-out/artifact"
  printf idle >"$sb/root-2/buck-out/artifact"
  cat >"$sb/buck2" <<'EOF'
#!/usr/bin/env bash
printf '%s:%s\n' "$PWD" "$*" >>"$CALLS"
if [[ "$1" == audit ]]; then printf '{"buck2.defer_write_actions":"true"}\n'; fi
if [[ "$PWD" == *'/root-1' && "$*" == *--dry-run* ]]; then exit 7; fi
EOF
  chmod +x "$sb/buck2"
  cat >"$sb/fakebin/git" <<EOF
#!/usr/bin/env bash
case "\$*" in
  *"worktree list --porcelain"*) printf 'worktree %s/root-1\\n\\nworktree %s/root-2\\n' "$sb" "$sb" ;;
  *"rev-parse --git-common-dir"*) printf '%s/.git\\n' "$sb" ;;
  *"rev-parse --git-dir"*) case "\$*" in *root-1*) printf '%s/.git/worktrees/root-1\\n' "$sb" ;; *root-2*) printf '%s/.git/worktrees/root-2\\n' "$sb" ;; *) printf '%s/.git\\n' "$sb" ;; esac ;;
  *"rev-parse --show-toplevel"*) printf '%s\\n' "\$2" ;;
  *"status --porcelain"*) : ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$sb/fakebin/git"
  cat >"$sb/fakebin/df" <<'EOF'
#!/usr/bin/env bash
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf 'disk 100000 95000 5000 95%% /\n'
EOF
  chmod +x "$sb/fakebin/df"
  run_script "$sb" rue-storage guard --finished-root "$sb/root-1" --finished-root "$sb/root-2" || rc=$?
  check "storage: active evidence is skipped while idle evidence is named" "$([ "$rc" -ne 0 ] && grep -q "root-2" "$sb/out.log" && ! grep -q "root-1.*eligible" "$sb/out.log" && echo 0 || echo 1)"
  rm -rf "$sb"
}

# --- run everything ---------------------------------------------------------

test_jjtidy_gh_failure_deletes_nothing
test_jjtidy_only_deletes_proven_merged
test_storage_git_failure_is_fail_closed
test_storage_plans_every_registered_root
test_storage_guard_is_host_wide_only_under_pressure
test_storage_guard_preserves_incremental_rustc_scaffolding
test_storage_guard_blocks_when_pressure_remains_critical
test_storage_guard_proceeds_between_cleanup_and_hard_floors
test_storage_guard_survives_cleanup_failure_between_floors
test_storage_guard_still_refuses_below_hard_floor_when_cleanup_fails
test_storage_reset_validates_all_targets_first
test_storage_reclaim_finished_preserves_dirty_and_clean_sources
test_storage_reclaim_finished_rejects_invalid_and_current_targets
test_storage_reclaim_finished_rejects_replacement_clone_identity
test_storage_reclaim_finished_ignores_ambient_git_selection
test_storage_reclaim_finished_rejects_main_metadata_counterfeit
test_storage_reclaim_finished_rejects_same_common_counterfeit
test_storage_reclaim_finished_rejects_isolation_symlink_escape
test_storage_reclaim_finished_rejects_non_directory_output_root
test_storage_reclaim_finished_rejects_output_root_swap
test_storage_reclaim_finished_rejects_regular_output_root_replacement
test_storage_reclaim_finished_rejects_initial_probe_replacement
test_storage_reclaim_finished_rejects_initial_probe_isolation_replacement
test_storage_reclaim_finished_rejects_initial_probe_new_isolation
test_storage_reclaim_finished_rejects_regular_isolation_replacement
test_storage_reclaim_finished_rejects_worktree_replacement
test_storage_reclaim_finished_rejects_valid_same_path_replacement
test_storage_reclaim_finished_handles_empty_isolation_that_gains_output
test_storage_reclaim_finished_cleans_zero_byte_output_entry
test_storage_reclaim_finished_rechecks_empty_root_after_later_preflight
test_storage_reclaim_finished_rejects_new_isolation_after_clean
test_storage_reclaim_finished_reports_buck_owned_residual
test_storage_reclaim_finished_reports_non_directory_residual
test_storage_reclaim_finished_rejects_root_replacement_after_clean
test_storage_reclaim_finished_explicitly_scopes_v2_with_environment_override
test_storage_reclaim_finished_alias_active_and_zero_output
test_storage_reclaim_finished_alias_and_preflight_validate_all
test_storage_reclaim_finished_scopes_every_isolation
test_storage_guard_finished_evidence_rechecks_output_after_ttl_cleanup
test_storage_guard_finished_evidence_rejects_root_replacement
test_storage_guard_rejects_malformed_evidence_and_reclaim_rejects_unknown_source
test_storage_guard_names_only_caller_supplied_finished_evidence
test_storage_guard_skips_active_evidence_and_names_idle_evidence

echo "--------------------------------------------------"
if [ "$FAILURES" -eq 0 ]; then
  echo "cleanup-script tests: all $TESTS checks passed"
  exit 0
else
  echo "cleanup-script tests: $FAILURES of $TESTS checks FAILED"
  exit 1
fi
