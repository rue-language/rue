#!/usr/bin/env python3
"""Per-target over-declaration report for rue_program (ADR-0070 / RUE-1404).

Advisory ONLY — the correctness half (under-declaration) is enforced in-band by
rue-program-derive-manifest.py. This report lists declared srcs the scan never
read. It deliberately does NOT classify them: sibling roots legitimately share
a glob, so one target's unread file may be another target's whole tree; the
directory-wide judgement belongs to rue-program-family-report.py. Attached as
an OPTIONAL ValidationInfo validation, so it never runs (or fails) an ordinary
build; select it with --enable-optional-validations srcs-precision.

Comparison is over path sets, never envelope bytes: the envelope embeds
device/inode identity and mtimes and is not machine-stable.
"""

import argparse
import json
import posixpath
import os


def normalize(path: str) -> str:
    return posixpath.normpath(path.replace(os.sep, "/"))


def accepted_relative(envelope: dict, root_rel: str) -> set:
    root_dir_rel = posixpath.dirname(normalize(root_rel)) or "."
    project_root = normalize(envelope["context"]["project_root"])
    reads = set()
    for read in envelope.get("accepted_reads", []):
        path = normalize(read["requested_path"])
        if path == project_root or path.startswith(project_root + "/"):
            suffix = path[len(project_root):].lstrip("/")
            reads.add(normalize(posixpath.join(root_dir_rel, suffix)))
    return reads


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--envelope", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--srcs-list", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    with open(args.envelope, encoding="utf-8") as handle:
        envelope = json.load(handle)
    with open(args.srcs_list, encoding="utf-8") as handle:
        srcs = {normalize(line.strip()) for line in handle if line.strip()}

    unread = sorted(srcs - accepted_relative(envelope, args.root))
    message = (
        "every declared src was read by the scan"
        if not unread
        else "declared but unread by this target (a sibling may read them; "
        "see the family aggregate): " + ", ".join(unread)
    )
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(
            {"version": 1, "data": {"status": "success", "message": message}},
            handle,
        )


if __name__ == "__main__":
    main()
