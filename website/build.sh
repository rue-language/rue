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
    # Loud on purpose. An empty page is honest only when there is genuinely
    # nothing collected; once the branch exists, failing to read it and
    # publishing an empty dashboard would overwrite real measurements with a
    # claim that none were taken.
    if ! git --work-tree="$PERF_DATA_ROOT" checkout origin/performance-data-v1 -- . 2>/dev/null; then
        git reset >/dev/null 2>&1 || true
        echo "  error: performance-data-v1 exists but could not be read" >&2
        exit 1
    fi
    git reset >/dev/null 2>&1 || true
    echo "  read $(find "$PERF_DATA_ROOT/runs" -name '*.json' 2>/dev/null | wc -l | tr -d ' ') run object(s)"
else
    echo "  (no performance-data-v1 branch yet; the page will render empty)"
fi

BENCH="$("$ROOT/buck2" build //crates/rue-bench:rue-bench --show-simple-output 2>/dev/null | tail -1)"
if [ -n "$BENCH" ] && [ -x "$BENCH" ]; then
    # Both record kinds are derived by one call over one store: ADR-0067's
    # compiler series and ADR-0072's runtime series. The runtime section is
    # absent from the output until this flag is passed, so the page can tell
    # "not derived" from "derived and empty".
    "$BENCH" derive \
        --manifest "$ROOT/performance/manifest.toml" \
        --runtime-manifest "$ROOT/performance/runtime.toml" \
        --data-root "$PERF_DATA_ROOT" \
        --out "$ROOT/website/static/performance-data.json"
    # Tooltips need each commit's subject and its position on trunk, neither of
    # which the raw records carry: a run object stores what was measured, not
    # how the commit was described nor where it sat in history. Resolving them
    # here keeps the derivation free of git.
    python3 "$ROOT/scripts/annotate-performance-commits.py" \
        --data "$ROOT/website/static/performance-data.json" \
        --repo "$ROOT"
else
    echo "  (rue-bench unavailable; writing an empty dataset)"
    printf '{"bands":[],"workloads":[],"platforms":[],"rejected":[]}\n' \
        > "$ROOT/website/static/performance-data.json"
fi
rm -rf "$PERF_DATA_ROOT"

# Rebuild the homepage Field Report from the repository (RUE-1261).
#
# The board asserts that this project can be checked rather than trusted, so
# every figure on it is computed here. Both inputs are optional and the page
# drops the rows it cannot fill: a spec figure needs the traceability report,
# and the performance index needs a published baseline that does not exist
# until collection has run.
echo "Deriving homepage status..."
TRACEABILITY_JSON="$(mktemp)"
SPEC_BIN="$("$ROOT/buck2" build //crates/rue-spec:rue-spec --show-simple-output 2>/dev/null | tail -1)"
if [ -n "$SPEC_BIN" ] && [ -x "$SPEC_BIN" ]; then
    # `--json` exits 0 even on a coverage gap: failing that gap is the
    # traceability gate's job, and a red gate must not also break the site.
    RUE_SPEC_DIR="$ROOT/docs/spec/src" RUE_SPEC_CASES="$ROOT/crates/rue-spec/cases" \
        "$SPEC_BIN" --traceability --json > "$TRACEABILITY_JSON" 2>/dev/null || true
else
    echo "  (rue-spec unavailable; the spec rows will be omitted)"
fi
python3 "$ROOT/scripts/generate-site-status.py" \
    --out "$ROOT/website/static/status.json" \
    --repo "$ROOT" \
    --traceability "$TRACEABILITY_JSON" \
    --performance-data "$ROOT/website/static/performance-data.json"
rm -f "$TRACEABILITY_JSON"

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
