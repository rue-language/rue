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
