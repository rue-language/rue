#!/usr/bin/env bash
# Run Lean Comparator on the statement/proof split (RUE-2460; README, "The
# statement layer").
#
# usage: comparator/run.sh               (sandboxed; refuses unless the sandbox is shown to work)
#        comparator/run.sh --unsandboxed (no sandbox, said loudly; macOS, bin/chain.sh)
#        (from anywhere; runs in docs/formal/lean)
#
# Builds Comparator and lean4export at the revisions that match this package's
# toolchain into $COMPARATOR_HOME (default: .lake/comparator), then runs
# Comparator on comparator/config.json: challenge `Challenge` (every Spec
# statement written out, with `sorry` proofs), solution `RueCore.Spine`, the
# 36 spine theorems, axioms `propext` and `Quot.sound` only.
#
# Comparator sandboxes every build and export with `landrun`, which needs Linux
# Landlock; put a `landrun` built from its `main` branch in PATH (or set
# COMPARATOR_LANDRUN). Comparator calls it with `--best-effort`, which on a
# kernel without Landlock runs the command unsandboxed and says nothing, so the
# default mode runs Comparator only on positive evidence that this landrun
# sandboxes (`sandbox_works` below), and refuses otherwise.
#
# `--unsandboxed` uses Comparator's own scripts/fake-landrun.sh instead, which
# runs the same steps with no sandbox at all: every check is made, but a
# malicious solution could tamper with the build. That is how it runs on macOS,
# and it is enough for our own proofs; the sandbox is what defends against a
# solution written to game the checker. (`--fake-landrun` is the old spelling.)
set -euo pipefail
cd "$(dirname "$0")/.."

toolchain=$(cat lean-toolchain)             # leanprover/lean4:v4.33.1
home=${COMPARATOR_HOME:-$PWD/.lake/comparator}
rev=v4.33.0   # Comparator's tag for Lean 4.33; its manifest pins lean4export v4.33.0

if [ ! -x "$home/.lake/build/bin/comparator" ]; then
  rm -rf "$home"
  git clone --quiet https://github.com/leanprover/comparator.git "$home"
  git -C "$home" checkout --quiet "$rev"
  # lean4export reads .olean files, which must be this toolchain's exactly
  echo "$toolchain" > "$home/lean-toolchain"
  (cd "$home" && lake build lean4export comparator)
fi

export COMPARATOR_LEAN4EXPORT=$home/.lake/packages/lean4export/.lake/build/bin/lean4export

# Positive evidence that "$1" (a landrun) sandboxes, called the way Comparator
# calls it (`--best-effort`, `--ldd`, `--add-exec`, grants by path). Three
# checks, each needed:
#  1. control: unsandboxed, /bin/sh can write the probe file and read this script;
#  2. the landrun runs /bin/sh granted write access to one directory, and the
#     write there succeeds — a landrun that always fails, or is too old for
#     these flags, fails here rather than passing as "denied";
#  3. the same landrun, same grants, is denied both a write to a second
#     directory (the file must not exist afterwards, whatever the exit code)
#     and a read of this script, which exists and is not granted.
# A shim that execs its command, or a landrun on a kernel without Landlock,
# lets 3 through; one that exits non-zero fails 2. Any doubt refuses.
sandbox_works() {
  local lr=$1 dir ok=1
  dir=$(mktemp -d "${TMPDIR:-/tmp}/comparator-probe.XXXXXX") || return 1
  mkdir -p "$dir/granted" "$dir/forbidden"
  local script=$PWD/comparator/run.sh
  # 1. control
  if ! /bin/sh -c 'echo x > "$1" && cat "$2" > /dev/null' sh "$dir/forbidden/control" "$script" \
      || [ ! -s "$dir/forbidden/control" ]; then
    echo "comparator/run.sh: sandbox probe: the unsandboxed control failed (cannot write $dir/forbidden or read $script)" >&2
    ok=0
  fi
  rm -f "$dir/forbidden/control"
  # 2. the granted write must succeed under the sandbox
  if [ $ok = 1 ]; then
    if ! "$lr" --best-effort --ldd --add-exec --rw "$dir/granted" -- \
        /bin/sh -c 'echo x > "$1"' sh "$dir/granted/allowed" >/dev/null 2>&1 \
        || [ ! -s "$dir/granted/allowed" ]; then
      echo "comparator/run.sh: sandbox probe: '$lr' did not run a granted write (it exits non-zero, is not landrun, or is too old for --best-effort/--ldd/--add-exec)" >&2
      ok=0
    fi
  fi
  # 3. the ungranted write and read must be denied
  if [ $ok = 1 ]; then
    "$lr" --best-effort --ldd --add-exec --rw "$dir/granted" -- \
      /bin/sh -c 'echo x > "$1"' sh "$dir/forbidden/escaped" >/dev/null 2>&1 || true
    if [ -e "$dir/forbidden/escaped" ]; then
      echo "comparator/run.sh: sandbox probe: '$lr' let a sandboxed process write outside its grants ($dir/forbidden)" >&2
      ok=0
    fi
    if "$lr" --best-effort --ldd --add-exec --rw "$dir/granted" -- \
        /bin/sh -c 'cat "$1"' sh "$script" >/dev/null 2>&1; then
      echo "comparator/run.sh: sandbox probe: '$lr' let a sandboxed process read outside its grants ($script)" >&2
      ok=0
    fi
  fi
  rm -rf "$dir"
  [ $ok = 1 ]
}

case "${1:-}" in
  --unsandboxed|--fake-landrun)
    export COMPARATOR_LANDRUN=$home/scripts/fake-landrun.sh
    echo "comparator/run.sh: *** UNSANDBOXED *** Comparator's builds and exports run with no sandbox (fake-landrun): its checks are all made, but a solution written to tamper with the build is not contained" >&2
    ;;
  "")
    if [ -z "${COMPARATOR_LANDRUN:-}" ]; then
      if ! COMPARATOR_LANDRUN=$(command -v landrun); then
        echo "comparator/run.sh: refusing: no landrun in PATH (it needs Linux Landlock); pass --unsandboxed to run with no sandbox, knowingly" >&2
        exit 1
      fi
    fi
    if ! sandbox_works "$COMPARATOR_LANDRUN"; then
      echo "comparator/run.sh: refusing: '$COMPARATOR_LANDRUN' is not shown to sandbox (above), so Comparator's sandbox could be a no-op; use landrun on a kernel with Landlock, or --unsandboxed to run with no sandbox, knowingly" >&2
      exit 1
    fi
    export COMPARATOR_LANDRUN
    echo "comparator/run.sh: sandbox probe passed: '$COMPARATOR_LANDRUN' ran a granted write and was denied an ungranted write and read" >&2
    ;;
  *)
    echo "usage: comparator/run.sh [--unsandboxed]" >&2
    exit 2
    ;;
esac

exec lake env "$home/.lake/build/bin/comparator" comparator/config.json
