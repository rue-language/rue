#!/usr/bin/env python3
"""Normalize BTD `--json-lines` output to bare Buck target labels, one per line.

BTD emits one JSON object per impacted target with (among others) a "target"
field holding the fully-qualified label (e.g. "root//:spec-tests"). This reads
that stream on stdin and prints the normalized labels (leading "root" cell
stripped, so "root//:spec-tests" and "//:spec-tests" compare equal) that
scripts/affected-targets intersects with its selectable-corpus set.

Malformed lines are skipped rather than fatal: the caller treats a parse failure
(non-zero exit) as a reason to fall back to a full run, and skipping the odd
unparseable line must not defeat matching the lines we can read.
"""

import json
import sys


def normalize(label: str) -> str:
    # "root//:spec-tests" -> "//:spec-tests"; leave "//:spec-tests" and
    # "cell//pkg:name" otherwise untouched.
    if label.startswith("root//"):
        return label[len("root"):]
    return label


def main() -> int:
    seen = []
    seen_set = set()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = obj.get("target")
        if not isinstance(target, str):
            continue
        norm = normalize(target)
        if norm not in seen_set:
            seen_set.add(norm)
            seen.append(norm)
    for label in seen:
        print(label)
    return 0


if __name__ == "__main__":
    sys.exit(main())
