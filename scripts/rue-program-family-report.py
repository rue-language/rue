#!/usr/bin/env python3
"""Directory-scoped over-declaration report for a rue_program family.

The aggregate half of ADR-0070's precision audit (RUE-1404): a per-target
report cannot judge an unread file (a sibling may read it), so this script
unions the accepted-read sets of every sibling program sharing a glob and
reports files that NO sibling reads — those are dead weight in every action
key they touch. Attached as an OPTIONAL ValidationInfo validation on the
family's `-srcs-report` target: it fails (status "failure") only when someone
opted in with --enable-optional-validations and dead files exist, which is the
advisory-but-meaningful shape the ADR specifies.

Arguments arrive as repeated (--envelope, --srcs-list, --root) triples, one
per sibling, in the family macro's declaration order.
"""

import argparse
import json
import os
import posixpath


def normalize(path: str) -> str:
    return posixpath.normpath(path.replace(os.sep, "/"))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--envelope", action="append", required=True)
    parser.add_argument("--srcs-list", action="append", required=True)
    parser.add_argument("--root", action="append", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    if not (len(args.envelope) == len(args.srcs_list) == len(args.root)):
        raise SystemExit("mismatched --envelope/--srcs-list/--root triples")

    declared = set()
    read_by_any = set()
    for envelope_path, srcs_path, root in zip(args.envelope, args.srcs_list, args.root):
        with open(srcs_path, encoding="utf-8") as handle:
            declared.update(normalize(line.strip()) for line in handle if line.strip())
        with open(envelope_path, encoding="utf-8") as handle:
            envelope = json.load(handle)
        project_root = normalize(envelope["context"]["project_root"])
        root_dir_rel = posixpath.dirname(normalize(root)) or "."
        for accepted in envelope.get("accepted_reads", []):
            path = normalize(accepted["requested_path"])
            if path == project_root or path.startswith(project_root + "/"):
                suffix = path[len(project_root):].lstrip("/")
                read_by_any.add(normalize(posixpath.join(root_dir_rel, suffix)))

    dead = sorted(declared - read_by_any)
    if dead:
        payload = {
            "status": "failure",
            "message": "declared by the family but read by no sibling program "
            "(dead weight in every action key): " + ", ".join(dead),
        }
    else:
        payload = {
            "status": "success",
            "message": "every declared file is read by at least one sibling",
        }
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump({"version": 1, "data": payload}, handle)


if __name__ == "__main__":
    main()
