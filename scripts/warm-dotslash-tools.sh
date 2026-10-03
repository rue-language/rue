#!/usr/bin/env bash
set -euo pipefail

# Populate DotSlash's verified artifact store before correctness or site work.
# `fetch` verifies the manifest digest and prints the executable path without
# running the downloaded tool, keeping warming outside measured/build work.
case "${1:-}" in
    peers)
        echo "Warming pinned Hugo artifact..."
        dotslash -- fetch ./hugo
        echo "Warming pinned Zola artifact..."
        dotslash -- fetch ./zola
        ;;
    website)
        echo "Warming pinned Tailwind artifact..."
        dotslash -- fetch ./tailwindcss
        ;;
    *)
        echo "usage: scripts/warm-dotslash-tools.sh peers|website" >&2
        exit 2
        ;;
esac
