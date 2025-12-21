#!/usr/bin/env bash
set -euo pipefail

# Build the Rue website with the spec included
# Usage: ./build.sh [serve]

cd "$(dirname "$0")/.."
ROOT="$PWD"

# Build the mdbook-spec preprocessor with buck2
echo "Building mdbook-spec preprocessor..."
MDBOOK_SPEC="$(./buck2 build //docs/spec/tools/mdbook-spec:mdbook-spec --show-output | tail -1 | awk '{print $2}')"
MDBOOK_SPEC_DIR="$ROOT/$(dirname "$MDBOOK_SPEC")"

# Create symlink with hyphenated name (buck2 uses underscores)
ln -sf mdbook_spec "$MDBOOK_SPEC_DIR/mdbook-spec"

# Build the spec
echo "Building specification..."
export PATH="$MDBOOK_SPEC_DIR:$PATH"
cd docs/spec && "$ROOT/mdbook" build
cd "$ROOT"

# Copy spec into website static
echo "Copying spec to website/static/spec..."
rm -rf website/static/spec
mkdir -p website/static/spec
cp -r docs/spec/book/* website/static/spec/

# Build or serve
cd website
if [[ "${1:-}" == "serve" ]]; then
    echo "Starting dev server at http://127.0.0.1:1111"
    "$ROOT/zola" serve
else
    echo "Building website..."
    "$ROOT/zola" build
    echo "Done! Output in website/public/"
fi
