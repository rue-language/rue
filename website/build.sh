#!/usr/bin/env bash
set -euo pipefail

# Build the Rue website
# Usage: ./build.sh [serve]

cd "$(dirname "$0")/.."
ROOT="$PWD"

# Platforms to generate charts for
PLATFORMS=("x86-64-linux" "aarch64-linux" "aarch64-macos")

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

# Generate benchmark charts for each platform
echo "Generating benchmark charts..."
BENCHMARKS_DIR="$ROOT/website/static/benchmarks"
mkdir -p "$BENCHMARKS_DIR/platforms"
mkdir -p "$BENCHMARKS_DIR/comparison"

# Track which platforms have data for the root metadata.json
PLATFORMS_WITH_DATA=()

for platform in "${PLATFORMS[@]}"; do
    history_file="$BENCHMARKS_DIR/history-${platform}.json"
    platform_dir="$BENCHMARKS_DIR/platforms/${platform}"

    if [[ -f "$history_file" ]]; then
        echo "  Generating charts for ${platform}..."
        mkdir -p "$platform_dir"
        python3 "$ROOT/scripts/generate-charts.py" \
            "$history_file" \
            "$platform_dir" \
            --platform "$platform"
        PLATFORMS_WITH_DATA+=("$platform")
    else
        echo "  No history file for ${platform} (skipping)"
    fi
done

# Generate comparison charts if we have multiple platforms
if [[ ${#PLATFORMS_WITH_DATA[@]} -gt 0 ]]; then
    echo "  Generating comparison charts..."
    history_files=()
    for platform in "${PLATFORMS_WITH_DATA[@]}"; do
        history_files+=("$BENCHMARKS_DIR/history-${platform}.json")
    done
    python3 "$ROOT/scripts/generate-charts.py" \
        --comparison \
        "$BENCHMARKS_DIR/comparison" \
        "${history_files[@]}"
fi

# Generate root metadata.json listing all platforms
echo "  Generating root metadata.json..."
python3 -c "
import json
import os
from pathlib import Path

benchmarks_dir = Path('$BENCHMARKS_DIR')
platforms = []

for platform in ${PLATFORMS_WITH_DATA[@]+"${PLATFORMS_WITH_DATA[@]}"}:
    platform = platform.strip()
    if not platform:
        continue
    metadata_file = benchmarks_dir / 'platforms' / platform / 'metadata.json'
    if metadata_file.exists():
        with open(metadata_file) as f:
            data = json.load(f)
        platforms.append({
            'id': platform,
            'name': data.get('platform_name', platform),
            'has_data': True,
            'run_count': data.get('run_count', 0),
            'latest_commit': data.get('latest_commit')
        })

# Add platforms without data
all_platforms = ['x86-64-linux', 'aarch64-linux', 'aarch64-macos']
platform_ids = [p['id'] for p in platforms]
for platform in all_platforms:
    if platform not in platform_ids:
        platforms.append({
            'id': platform,
            'name': {'x86-64-linux': 'Linux x86-64', 'aarch64-linux': 'Linux ARM64', 'aarch64-macos': 'macOS ARM64'}.get(platform, platform),
            'has_data': False
        })

# Sort by platform id for consistency
platforms.sort(key=lambda p: p['id'])

metadata = {
    'platforms': platforms,
    'default_platform': platforms[0]['id'] if platforms else None
}

with open(benchmarks_dir / 'metadata.json', 'w') as f:
    json.dump(metadata, f, indent=2)
" 2>/dev/null || echo "  (No platform data available)"

# Backwards compatibility: Generate charts from legacy history.json if it exists
# and no per-platform history exists yet
if [[ -f "$BENCHMARKS_DIR/history.json" && ${#PLATFORMS_WITH_DATA[@]} -eq 0 ]]; then
    echo "  Generating legacy charts from history.json..."
    python3 "$ROOT/scripts/generate-charts.py" \
        "$BENCHMARKS_DIR/history.json" \
        "$BENCHMARKS_DIR/"
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
