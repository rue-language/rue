#!/usr/bin/env python3
"""Normalize JSON-lines target streams to bare Buck target labels, one per line.

Two input shapes, selected by flag, because they are NOT the same:

  (default)        BTD `--json-lines` output: one object per impacted target
                   with a "target" field holding the fully-qualified label,
                   e.g. {"target": "root//:spec-tests"}.
  --targets-dump   `buck2 targets --json-lines` output, the head-graph dump
                   scripts/affected-targets feeds to BTD: one object per
                   target carrying "buck.package" (e.g. "root//crates/rue")
                   and "name", with no "target" field at all.

Both print normalized labels (leading "root" cell stripped, so
"root//:spec-tests" and "//:spec-tests" compare equal) that
scripts/affected-targets intersects with its selectable-corpus set and with
the live head graph.

The distinction is load-bearing. The head dump was once parsed with the BTD
shape; every record failed "missing or non-string target", the caller took
that as its fail-open cue, and every pull request decided FULL for a week
while the selection tests passed against a fake buck2 that emitted BTD-shaped
records for `targets`. A silent fail-open is the one failure this script must
not have, so each mode rejects the other's shape instead of tolerating it.

Any non-empty malformed line, JSON value that is not an object, or object
missing the fields its mode requires is fatal. The caller treats a non-zero
exit as a reason to run the full corpus; accepting partial output could
under-select.
"""

import json
import sys


def normalize(label: str) -> str:
    # "root//:spec-tests" -> "//:spec-tests"; leave "//:spec-tests" and
    # "cell//pkg:name" otherwise untouched.
    if label.startswith("root//"):
        return label[len("root"):]
    return label


def btd_label(obj: dict) -> "str | None":
    target = obj.get("target")
    if not isinstance(target, str):
        raise ValueError("missing or non-string target")
    return target


def targets_dump_label(obj: dict) -> "str | None":
    # `--keep-going` turns a package that failed to evaluate into a record
    # carrying "buck.error" instead of targets. A head graph with a broken
    # package is not a graph to narrow against; refuse it so the caller runs
    # everything.
    if "buck.error" in obj:
        raise ValueError(f"head graph package failed to evaluate: {obj.get('buck.package')!r}")
    # `--imports` adds one package-level record per BUCK file, with
    # "buck.file" and "buck.imports" and no "name". It is not a target.
    if "name" not in obj and "buck.file" in obj:
        return None
    package = obj.get("buck.package")
    name = obj.get("name")
    if not isinstance(package, str) or not isinstance(name, str):
        raise ValueError("missing or non-string buck.package/name")
    # "root//crates/rue" + "rue" -> "root//crates/rue:rue"; the root package
    # is spelled "root//", which already ends in the separator.
    return f"{package}:{name}"


def main(argv: list) -> int:
    if argv == ["--targets-dump"]:
        label_of = targets_dump_label
    elif not argv:
        label_of = btd_label
    else:
        print("usage: parse-btd-impacted.py [--targets-dump] < stream", file=sys.stderr)
        return 2
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
        try:
            label = label_of(obj)
        except ValueError as error:
            print(f"parse-btd-impacted: {error}", file=sys.stderr)
            return 1
        if label is None:
            continue
        norm = normalize(label)
        if norm not in seen_set:
            seen_set.add(norm)
            seen.append(norm)
    for label in seen:
        print(label)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
