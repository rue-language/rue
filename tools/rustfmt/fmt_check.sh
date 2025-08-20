#!/usr/bin/env bash
set -euo pipefail

edition="${1:-2024}"

# Prefer git for speed and correctness; fall back to find if not a git repo.
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  mapfile -t files < <(git ls-files 'crates/**/*.rs')
else
  mapfile -t files < <(find crates -type f -name '*.rs')
fi

if [[ ${#files[@]} -eq 0 ]]; then
  echo "No Rust files under crates/."
  exit 0
fi

echo "Checking format of ${#files[@]} files (edition=$edition)…"
exec rustfmt --edition="$edition" --check "${files[@]}"
