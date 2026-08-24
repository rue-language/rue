#!/bin/bash
set -euo pipefail

# Fails when a crate's captured clippy diagnostics contain an error.
#
# Buck's Rust rules treat clippy as "infallible diagnostics": the
# `[clippy.txt]` subtarget always succeeds and merely *captures* clippy's
# output into a file, so building it can never fail a build. Something has to
# read that file and decide. This script is that decision, invoked per crate by
# the `<name>-clippy` test the rue_crate/rue_binary macros emit (RUE-1153).
#
# Why a test rather than the old top-level clippy.sh: a test declares the
# diagnostics file as an input, so Buck must materialize it before the test can
# run. clippy.sh had to pass --materializations=all precisely because a remote
# cache hit could satisfy its build without ever writing the file to disk,
# leaving it to score unread targets as clean (RUE-1152).
#
# Only `error:` lines gate. deny_lints=["warnings"] in toolchains/rust/BUCK
# turns every real clippy finding into an `error:` line, whereas rustc emits
# some `warning:` notes unconditionally (e.g. -Ctarget-feature) that ordinary
# builds also print and that must not fail the gate.

if [ "$#" -ne 1 ]; then
    echo "usage: clippy-gate.sh <clippy.txt>" >&2
    exit 2
fi

DIAGNOSTICS="$1"

# A missing file is not an empty file. Treating the two alike is what let
# unread diagnostics pass as clean; refuse to certify what we cannot read.
if [ ! -e "$DIAGNOSTICS" ]; then
    echo "clippy-gate.sh: diagnostics artifact does not exist: $DIAGNOSTICS" >&2
    exit 1
fi

# Strip ANSI colour codes so CI logs stay readable and grep stays reliable.
ESCAPE="$(printf '\033')"
CLEANED="$(sed -E "s/${ESCAPE}\\[[0-9;]*m//g" "$DIAGNOSTICS")"

# Herestring, not a pipe: under `pipefail` a matching `grep -q` exits early
# and kills the producer with EPIPE, failing the pipeline (RUE-1155).
if grep -qE '^error' <<<"$CLEANED"; then
    printf '%s\n' "$CLEANED"
    echo ""
    echo "clippy gate FAILED: lint violations above."
    echo "Fix them, or (if intentional) adjust the workspace lint policy in"
    echo "toolchains/rust/BUCK (deny_lints / allow_lints)."
    exit 1
fi

exit 0
