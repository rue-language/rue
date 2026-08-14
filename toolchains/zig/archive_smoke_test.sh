#!/usr/bin/env bash
set -euo pipefail

archive="$1"

test -s "$archive"

# A later `-g` would put the compilation unit and checkout in debug metadata,
# so scan the actual archive rather than trusting only the rule definition.
if grep -aEq '\.(debug|zdebug)_|__(debug|zdebug)_|/Users/|/home/|/workspace/|buck-out/' "$archive"; then
  echo "Zig archive contains debug metadata or a source/checkout path" >&2
  exit 1
fi
