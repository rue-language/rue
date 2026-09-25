#!/usr/bin/env bash
# Run Lean Comparator on the statement/proof split (RUE-2460; README, "The
# statement layer").
#
# usage: comparator/run.sh [--fake-landrun]     (from anywhere; runs in docs/formal/lean)
#
# Builds Comparator and lean4export at the revisions that match this package's
# toolchain into $COMPARATOR_HOME (default: .lake/comparator), then runs
# Comparator on comparator/config.json: challenge `Challenge` (the Spec
# statements with `sorry`), solution `RueCore.Spine`, the 36 spine theorems,
# axioms `propext` and `Quot.sound` only.
#
# Comparator sandboxes every build and export with `landrun`, which needs Linux
# Landlock; put a `landrun` built from its `main` branch in PATH (or set
# COMPARATOR_LANDRUN). `--fake-landrun` uses Comparator's own
# scripts/fake-landrun.sh instead, which runs the same steps unsandboxed: every
# check below is made, but a malicious solution could tamper with the build.
# That is how it runs on macOS, and it is enough for our own proofs; the
# sandbox is what defends against a solution written to game the checker.
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
if [ "${1:-}" = "--fake-landrun" ]; then
  export COMPARATOR_LANDRUN=$home/scripts/fake-landrun.sh
elif [ -z "${COMPARATOR_LANDRUN:-}" ]; then
  if ! COMPARATOR_LANDRUN=$(command -v landrun); then
    echo "comparator/run.sh: no landrun in PATH (it needs Linux Landlock); pass --fake-landrun to run unsandboxed" >&2
    exit 1
  fi
  export COMPARATOR_LANDRUN
fi
if [ "${1:-}" != "--fake-landrun" ]; then
  # Comparator calls landrun with --best-effort, which on a kernel without
  # Landlock runs the command unsandboxed and says nothing. Probe the same way:
  # a sandbox that grants only `cat` and its libraries must not read /etc/hostname
  if "$COMPARATOR_LANDRUN" --best-effort --ldd --add-exec -- cat /etc/hostname >/dev/null 2>&1; then
    echo "comparator/run.sh: landrun cannot apply Landlock on this kernel, so Comparator's sandbox would be a no-op; use a kernel with Landlock, or --fake-landrun to run unsandboxed knowingly" >&2
    exit 1
  fi
fi

exec lake env "$home/.lake/build/bin/comparator" comparator/config.json
