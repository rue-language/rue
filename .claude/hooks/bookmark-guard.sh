#!/usr/bin/env bash
# PreToolUse guard on Edit/Write: block edits while the working copy (@) sits
# on a pushed PR branch. Editing @ then pushing mutates a queued PR — this
# exact mistake put a blog post into PR #981's branch (2026-06-11) and cost
# twenty minutes of surgery. Deliberate flows are unaffected: bounce
# resolution uses `jj new <change>` + `jj squash` (where @ carries no
# bookmark), and `jj new 'trunk()'` is always the fix this hook suggests.
#
# Fail-open by design: any error (no jj, worktree checkout, unparseable
# input) exits 0 so the hook can never wedge unrelated work.
set -uo pipefail

input=$(cat) || exit 0
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)" || exit 0

file_path=$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("tool_input", {}).get("file_path", ""))
except Exception:
    pass
' 2>/dev/null) || exit 0

# Only guard files inside the main workspace; agent worktrees use git directly.
case "$file_path" in
  "$repo_root"/.claude/worktrees/*) exit 0 ;;
  "$repo_root"/*) ;;
  *) exit 0 ;;
esac
[ -d "$repo_root/.jj" ] || exit 0

bookmarks=$(cd "$repo_root" && jj log --no-graph -r @ -T 'bookmarks' 2>/dev/null) || exit 0
# Herestring, not a pipe: under `pipefail` a matching `grep -q` kills the
# producer with EPIPE and the pipeline reports failure, so the guard would stop
# blocking in exactly the case it exists to block (RUE-1155).
if grep -q 'push-' <<<"$bookmarks"; then
  echo "BLOCKED by bookmark-guard: the working copy (@) is on pushed PR branch '$bookmarks'. Editing it would mutate a queued PR. Run jj new 'trunk()' (new work) or jj new <change> + jj squash (deliberate amend) first." >&2
  exit 2
fi
exit 0
