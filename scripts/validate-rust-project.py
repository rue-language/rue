#!/usr/bin/env python3
"""Validate the checked-in rust-project.json against the repository (RUE-516).

Rue has no Cargo workspace, so rust-project.json IS the rust-analyzer project
model. It used to drift silently — referencing a deleted crate (`rue-intern`)
and omitting live ones — misdirecting anyone who opened the repo in an IDE. This
is a pure-filesystem check (no Buck needed) that fails when the model:

  * references a `root_module` that does not exist (e.g. a deleted crate),
  * omits a live first-party crate under `crates/`, or
  * is structurally malformed (bad dep index, unknown edition, etc.).

Regenerate with `./gen-rust-project.sh` when it complains. Run from the repo
root; exits non-zero with an explanation on any problem.
"""

from __future__ import annotations

import json
import os
import sys

VALID_EDITIONS = {"2015", "2018", "2021", "2024"}


def fail(errors: list[str]) -> None:
    print("rust-project.json validation FAILED:", file=sys.stderr)
    for e in errors:
        print(f"  - {e}", file=sys.stderr)
    print(
        "\nRegenerate with ./gen-rust-project.sh (and commit the result).",
        file=sys.stderr,
    )
    sys.exit(1)


def main() -> None:
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    os.chdir(repo_root)

    path = "rust-project.json"
    if not os.path.exists(path):
        fail([f"{path} does not exist"])

    try:
        with open(path) as f:
            model = json.load(f)
    except json.JSONDecodeError as e:
        fail([f"{path} is not valid JSON: {e}"])

    errors: list[str] = []
    crates = model.get("crates")
    if not isinstance(crates, list) or not crates:
        fail([f"{path} has no 'crates' array"])

    n = len(crates)
    for i, c in enumerate(crates):
        who = c.get("display_name", f"crate #{i}")
        root_module = c.get("root_module")
        if not isinstance(root_module, str) or not root_module:
            errors.append(f"{who}: missing root_module")
        elif not os.path.exists(root_module):
            errors.append(f"{who}: root_module does not exist: {root_module}")

        edition = c.get("edition")
        if edition not in VALID_EDITIONS:
            errors.append(f"{who}: invalid edition {edition!r}")

        for dep in c.get("deps", []):
            idx = dep.get("crate")
            if not isinstance(idx, int) or not (0 <= idx < n):
                errors.append(f"{who}: dep index out of range: {idx}")

    # First-party coverage: every crates/<dir> with a lib.rs/main.rs must be
    # represented by some crate rooted under it. This is the exact class of bug
    # that motivated RUE-516 (six live crates were silently absent).
    represented_dirs = set()
    for c in crates:
        rm = c.get("root_module", "")
        parts = rm.split("/")
        if len(parts) >= 2 and parts[0] == "crates":
            represented_dirs.add(parts[1])

    for entry in sorted(os.listdir("crates")):
        directory = os.path.join("crates", entry)
        if not os.path.isdir(directory):
            continue
        has_root = any(
            os.path.exists(os.path.join(directory, cand))
            for cand in ("src/lib.rs", "src/main.rs")
        )
        if has_root and entry not in represented_dirs:
            errors.append(
                f"first-party crate 'crates/{entry}' is missing from rust-project.json"
            )

    if errors:
        fail(errors)

    first_party = sum(
        1 for c in crates if c.get("root_module", "").startswith("crates/")
    )
    print(
        f"rust-project.json OK: {n} crates "
        f"({first_party} first-party), all root_modules exist."
    )


if __name__ == "__main__":
    main()
