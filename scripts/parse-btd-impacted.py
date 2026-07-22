#!/usr/bin/env python3
"""Normalize BTD `--json-lines` output to bare Buck target labels, one per line.

BTD emits one JSON object per impacted target with (among others) a "target"
field holding the fully-qualified label (e.g. "root//:spec-tests"). This reads
that stream on stdin and prints the normalized labels (leading "root" cell
stripped, so "root//:spec-tests" and "//:spec-tests" compare equal) that
scripts/affected-targets intersects with its selectable-corpus set.

Any non-empty malformed line, JSON value that is not an object, or object with
a missing/non-string target is fatal. The caller treats a non-zero exit as a
reason to run the full corpus; accepting partial output could under-select.
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
        except json.JSONDecodeError as error:
            print(f"parse-btd-impacted: malformed JSON: {error}", file=sys.stderr)
            return 1
        if not isinstance(obj, dict):
            print("parse-btd-impacted: JSON line is not an object", file=sys.stderr)
            return 1
        target = obj.get("target")
        if not isinstance(target, str):
            print("parse-btd-impacted: missing or non-string target", file=sys.stderr)
            return 1
        norm = normalize(target)
        if norm not in seen_set:
            seen_set.add(norm)
            seen.append(norm)
    for label in seen:
        print(label)
    return 0


if __name__ == "__main__":
    sys.exit(main())
