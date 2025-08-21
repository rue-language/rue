#!/usr/bin/env bash
set -euo pipefail

edition="${1:-2024}"

# Always use find to get actual files on disk
# This works correctly with both git and jj
mapfile -t files < <(find crates -type f -name '*.rs')

if [[ ${#files[@]} -eq 0 ]]; then
  echo "No Rust files under crates/."
  exit 0
fi

echo "Formatting ${#files[@]} files (edition=$edition)…"
exec rustfmt --edition="$edition" "${files[@]}"
