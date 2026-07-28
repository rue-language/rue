#!/usr/bin/env bash
set -euo pipefail

# Build the Rue website
# Usage: ./build.sh [serve]

cd "$(dirname "$0")/.."
ROOT="$PWD"

# Copy spec content into website/content/spec
# We use a copy rather than a symlink for Windows compatibility (symlinks
# require elevated privileges on Windows). The spec source lives in
# docs/spec/src/ to keep it near the compiler code.
echo "Copying spec content..."
rm -rf website/content/spec
cp -r docs/spec/src website/content/spec

# Rewrite internal links: spec files use @/XX-... but once copied to
# website/content/spec/, the links need to be @/spec/XX-...
echo "Rewriting spec internal links..."
if [[ "$OSTYPE" == "darwin"* ]]; then
    find website/content/spec -name "*.md" -exec sed -i '' 's|@/\([0-9]\)|@/spec/\1|g' {} \;
else
    find website/content/spec -name "*.md" -exec sed -i 's|@/\([0-9]\)|@/spec/\1|g' {} \;
fi

# Rebuild the performance dashboard's data from the raw records (ADR-0067).
#
# Everything derived — indexes, medians, dispersion, flags, phase composition —
# is recomputed here from `performance-data-v1` and never stored on that branch.
# The page renders honestly with nothing to show when collection has not run
# yet, which is its normal first state rather than an error.
echo "Deriving performance data..."
PERF_DATA_ROOT="$(mktemp -d)"
if git rev-parse --verify origin/performance-data-v1 >/dev/null 2>&1 \
   || git fetch origin performance-data-v1 --depth=50 >/dev/null 2>&1; then
    git --work-tree="$PERF_DATA_ROOT" checkout origin/performance-data-v1 -- . 2>/dev/null || true
    git reset >/dev/null 2>&1 || true
else
    echo "  (no performance-data-v1 branch yet; the page will render empty)"
fi

BENCH="$("$ROOT/buck2" build //crates/rue-bench:rue-bench --show-simple-output 2>/dev/null | tail -1)"
if [ -n "$BENCH" ] && [ -x "$BENCH" ]; then
    "$BENCH" derive \
        --manifest "$ROOT/performance/manifest.toml" \
        --data-root "$PERF_DATA_ROOT" \
        --out "$ROOT/website/static/performance-data.json"
    # Tooltips need commit subjects, which the raw records deliberately do not
    # carry: a run object stores what was measured, not how the commit was
    # described. Resolving them here keeps the derivation free of git.
    python3 "$ROOT/scripts/annotate-performance-subjects.py" \
        --data "$ROOT/website/static/performance-data.json" \
        --repo "$ROOT"
else
    echo "  (rue-bench unavailable; writing an empty dataset)"
    printf '{"bands":[],"workloads":[],"platforms":[],"rejected":[]}\n' \
        > "$ROOT/website/static/performance-data.json"
fi
rm -rf "$PERF_DATA_ROOT"

# Build Tailwind CSS
echo "Building Tailwind CSS..."
cd website
"$ROOT/tailwindcss" -i css/input.css -o static/style.css --minify

# Build or serve
if [[ "${1:-}" == "serve" ]]; then
    echo "Starting dev server at http://127.0.0.1:1111"
    echo "Note: CSS changes require rebuilding Tailwind manually"
    "$ROOT/zola" serve
else
    echo "Building website..."
    "$ROOT/zola" build
    echo "Done! Output in website/public/"
fi
