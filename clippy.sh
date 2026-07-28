#!/bin/bash
set -euo pipefail

# Clippy lint gate for the workspace (the CI `clippy` job runs this).
#
# Why not a single `buck2 build ...` that fails on lint? Buck2's rust rules run
# clippy with "infallible diagnostics": the `[clippy.txt]` subtarget always
# succeeds and merely *captures* clippy's output into a file (so downstream
# actions consuming diagnostics don't break). There is no built-in
# "fail-the-build-on-clippy" subtarget. So the gate is: build the
# `[clippy.txt]` subtarget for every first-party crate target, then fail if any
# of those diagnostic files is non-empty.
#
# The workspace lint policy lives in toolchains/rust/BUCK:
#   deny_lints  = ["warnings"]   -> every rustc/clippy warning is an error
#   allow_lints = _CLIPPY_ALLOW  -> grandfathered pre-existing clippy findings
# Shrink _CLIPPY_ALLOW over time; a lint removed from it becomes gating here.

cd "$(dirname "$0")"

echo "Enumerating first-party Rust targets..."
# One target per rust_library / rust_binary / rust_test under //crates/...
mapfile -t TARGETS < <(./buck2 uquery \
    "kind('rust_(library|binary|test)', 'root//crates/...')" 2>/dev/null)

if [ "${#TARGETS[@]}" -eq 0 ]; then
    echo "clippy.sh: no Rust targets found (uquery returned nothing)" >&2
    exit 1
fi

CLIPPY_TARGETS=()
for t in "${TARGETS[@]}"; do
    CLIPPY_TARGETS+=("${t}[clippy.txt]")
done

echo "Running clippy on ${#CLIPPY_TARGETS[@]} targets..."
# A real compile error (not a lint) makes this buck2 build fail non-zero -- that
# is correct, the tree is broken -- but the old `2>/dev/null` swallowed the
# compiler's diagnostics, so the gate failed with no explanation (RUE-550).
# Capture stderr and surface it on failure while keeping the success path quiet.
clippy_err="$(mktemp)"
trap 'rm -f "$clippy_err"' EXIT
clippy_status=0
# `--materializations=all` is load-bearing, not a tuning knob. A remote cache
# hit satisfies the build without ever writing the diagnostics file to local
# disk, and the loop below can only read files that exist -- so without this the
# gate scores cached targets as clean (RUE-1152).
SHOW_OUTPUT="$(./buck2 build "${CLIPPY_TARGETS[@]}" --materializations=all --show-output 2>"$clippy_err")" \
    || clippy_status=$?
if [ "$clippy_status" -ne 0 ]; then
    echo "clippy.sh: 'buck2 build' failed (exit $clippy_status) -- a real compile" >&2
    echo "error, not a lint. The tree is broken; diagnostics follow:" >&2
    cat "$clippy_err" >&2
    exit "$clippy_status"
fi

FAILED=0
# Each --show-output line is: "<target>[clippy.txt] <repo-root-relative-path>".
# We only fail on clippy *errors*: deny_lints=["warnings"] turns every real
# clippy finding into an `error:` line, whereas benign non-lint codegen output
# (e.g. `-Ctarget-feature` `warning:` notes that rustc emits unconditionally
# and that ordinary builds also print) must NOT gate. So grep for `^error`.
CHECKED=0
MISSING=0
while read -r TARGET ARTIFACT; do
    [ -z "${ARTIFACT:-}" ] && continue
    # A file that does not exist is not the same claim as a file that is empty.
    # The old `[ -s ... ] || continue` collapsed the two, so an unmaterialized
    # artifact was silently scored as "no findings" and the gate went green on
    # code it had never actually read (RUE-1152).
    if [ ! -e "$ARTIFACT" ]; then
        echo "clippy.sh: diagnostics artifact missing for $TARGET: $ARTIFACT" >&2
        MISSING=$((MISSING + 1))
        continue
    fi
    CHECKED=$((CHECKED + 1))
    [ -s "$ARTIFACT" ] || continue
    # Strip ANSI colour codes so CI logs stay readable and grep is reliable.
    CLEANED="$(sed -E 's/\x1b\[[0-9;]*m//g' "$ARTIFACT")"
    # Herestring, not a pipe: `grep -q` exits on its first match and closes the
    # pipe, so a piped producer dies of EPIPE and `pipefail` reports *that*
    # status for the whole pipeline. The `if` then reads false and the finding
    # is discarded. The failure is inverted -- it only fires when grep DID match
    # -- so a clean tree cannot distinguish this from a working gate (RUE-1155,
    # the same construct RUE-1011 removed from scripts/test-wrapper-scripts.sh).
    if grep -qE '^error' <<<"$CLEANED"; then
        FAILED=1
        echo "::group::clippy findings: $TARGET"
        printf '%s\n' "$CLEANED"
        echo "::endgroup::"
    fi
done <<< "$SHOW_OUTPUT"

# Coverage is part of the verdict. A gate that cannot say how many targets it
# read cannot distinguish "clean" from "never looked", which is precisely how
# 98 findings reached trunk under a green check.
if [ "$MISSING" -ne 0 ] || [ "$CHECKED" -ne "${#CLIPPY_TARGETS[@]}" ]; then
    echo "" >&2
    echo "clippy gate FAILED: read $CHECKED diagnostics files for ${#CLIPPY_TARGETS[@]}" >&2
    echo "enumerated targets ($MISSING missing). Refusing to report a pass for" >&2
    echo "targets whose diagnostics the gate never read." >&2
    exit 1
fi

if [ "$FAILED" -ne 0 ]; then
    echo ""
    echo "clippy gate FAILED: lint violations above."
    echo "Fix them, or (if intentional) adjust the workspace lint policy in"
    echo "toolchains/rust/BUCK (deny_lints / allow_lints)."
    exit 1
fi

echo "clippy gate passed: no lint violations."
