#!/usr/bin/env python3
"""Derive a machine-stable --source-manifest from a `rue --emit deps` envelope.

The derive step of ADR-0070's three-action `rue_program` shape (RUE-1404).
This script is where two load-bearing properties live, so a bug here is a
hermeticity bug, not a formatting bug:

  * THE DECLARED BOUNDARY IS ENFORCED HERE. Every accepted read outside
    `srcs ∪ std` fails the build (exit 1). A scan-derived manifest encodes
    whatever the scan observed, so without this check an out-of-srcs read
    would pass locally and be laundered into the action cache — the scan's
    key never mentions the stray file (ADR-0070, "Where hermeticity actually
    lives"). Do not "simplify" this script into writing what the scan saw.

  * MANIFEST ENTRIES ARE MACHINE-STABLE. The envelope's paths are absolute on
    the machine that ran the scan, and the scan action's result is uploaded to
    the shared cache; a manifest carrying those absolute paths would fail the
    compile on any machine with a different checkout root. Entries are
    re-anchored against the envelope's recorded project/std roots and written
    relative to the manifest's own directory (the compiler resolves relative
    entries against the manifest file's parent). test-rue-program-derive-
    manifest.py pins relocation invariance: two envelopes recorded under
    different roots must derive byte-identical manifests.

The manifest is accepted reads ∪ absent observations ∪ every file of the
declared std tree:

  * absent observations MUST be declared: resolution probes candidate arms in
    order and an undeclared probe is DeniedLexical-fatal even when a later,
    declared arm would resolve (ADR-0051; ADR-0070 "the manifest cannot come
    from a glob");
  * std MUST be unioned in unconditionally: trusted-std acquisition for
    fallible intrinsics happens only on semantic runs, so the scan cannot see
    it, and manifest membership means "available to import", not "read"
    (ADR-0047).

`--include-srcs` adds the declared srcs to that union for `rue_test`
(ADR-0083 / RUE-2004), whose run observes its `--test-candidates` inventory
under this manifest; see the comment at the call site. The boundary check is
unaffected either way.

`--expect-violation PATH` inverts the boundary check for negative control 1:
the script succeeds (writing a marker as the output) if and only if the
boundary check rejects exactly PATH. The control materializes its
out-of-srcs module as a hidden scan input so the failure stage is the same in
every execution environment (ADR-0070, "Negative controls").
"""

import argparse
import json
import os
import posixpath
import sys


def fail(message: str) -> "None":
    print(f"rue-program-derive-manifest: {message}", file=sys.stderr)
    sys.exit(1)


def read_list_file(path: str) -> list[str]:
    with open(path, encoding="utf-8") as handle:
        return [line.strip() for line in handle if line.strip()]


def normalize(path: str) -> str:
    """Lexical normalization, forward slashes, no filesystem access."""
    return posixpath.normpath(path.replace(os.sep, "/"))


def reanchor(abs_path: str, roots: list[tuple[str, str]]) -> "str | None":
    """Map an envelope-absolute path to a project-relative path.

    `roots` pairs the envelope's recorded absolute roots with the
    project-relative directories they correspond to in this build. Returns
    None when the path is under none of the roots — the caller decides
    whether that is a boundary violation (accepted reads) or ignorable.
    """
    path = normalize(abs_path)
    for abs_root, rel_root in roots:
        root = normalize(abs_root)
        if path == root or path.startswith(root + "/"):
            suffix = path[len(root):].lstrip("/")
            return normalize(posixpath.join(rel_root, suffix)) if suffix else rel_root
    return None


def walk_rue_files(directory: str) -> list[str]:
    found = []
    for base, _dirs, files in os.walk(directory):
        for name in files:
            if name.endswith(".rue"):
                found.append(normalize(os.path.join(base, name)))
    return sorted(found)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--envelope", required=True)
    parser.add_argument("--root", required=True, help="project-relative root source")
    parser.add_argument("--srcs-list", required=True, help="file of project-relative srcs")
    parser.add_argument("--std-dir", required=True, help="project-relative std root directory")
    parser.add_argument("--out", required=True, help="project-relative manifest output path")
    parser.add_argument(
        "--include-srcs",
        action="store_true",
        help="union the declared srcs into the manifest (rue_test)",
    )
    parser.add_argument("--expect-violation", default=None)
    args = parser.parse_args()

    with open(args.envelope, encoding="utf-8") as handle:
        envelope = json.load(handle)

    if envelope.get("status") != "complete":
        fail(f"scan envelope status is {envelope.get('status')!r}, expected 'complete'")

    context = envelope["context"]
    env_project_root = context["project_root"]
    env_std_root = context.get("std_root")

    root_rel = normalize(args.root)
    root_dir_rel = posixpath.dirname(root_rel) or "."
    std_dir_rel = normalize(args.std_dir)
    srcs = {normalize(p) for p in read_list_file(args.srcs_list)}

    # The envelope's project_root is the scan machine's absolute path of the
    # ROOT SOURCE's directory; std_root is its RUE_STD_PATH. These two anchors
    # are the only absolute prefixes a complete envelope can contain.
    roots = [(env_project_root, root_dir_rel)]
    if env_std_root:
        roots.append((env_std_root, std_dir_rel))

    violations = []
    entries = set()

    for read in envelope.get("accepted_reads", []):
        rel = reanchor(read["requested_path"], roots)
        if rel is None:
            violations.append(read["requested_path"])
            continue
        in_std = rel == std_dir_rel or rel.startswith(std_dir_rel + "/")
        if not in_std and rel not in srcs:
            violations.append(rel)
            continue
        entries.add(rel)

    for observation in envelope.get("observations", []):
        if observation["outcome"]["status"] != "absent":
            continue
        rel = reanchor(observation["request"]["requested_path"], roots)
        # An absent arm outside both roots cannot be spelled relative to the
        # project; the compiler will re-probe it via the same importer-relative
        # arithmetic, and a path that escapes the checkout entirely cannot be
        # granted by a manifest we can write. Skip rather than fail: nothing
        # was read.
        if rel is not None:
            entries.add(rel)

    if args.expect_violation is not None:
        expected = normalize(args.expect_violation)
        if violations == [expected]:
            with open(args.out, "w", encoding="utf-8") as handle:
                handle.write(f"boundary-violation-detected {expected}\n")
            return
        fail(
            "expected exactly one boundary violation "
            f"{expected!r}, observed {violations!r}"
        )

    if violations:
        details = "\n".join(f"  {path}" for path in sorted(violations))
        fail(
            "the compiler read files the rule does not declare "
            f"(outside srcs and std):\n{details}\n"
            "Declare them in srcs, or remove the import."
        )

    # `rue_test` additionally declares every src (ADR-0083 / RUE-2004). The
    # boundary check above is unchanged — an accepted read outside srcs ∪ std
    # still fails the build — so this widens the read policy only to files the
    # target already declares and Buck already materializes and keys. It is
    # what a candidate inventory needs to mean anything: `--test-candidates`
    # entries are observed UNDER the manifest, so an orphan test file, which by
    # definition is not an accepted read, would otherwise be unreadable and
    # reported as "could not be parsed" with no test count — the same warning
    # for an unimported file as for a corrupt one.
    if args.include_srcs:
        entries.update(srcs)

    # Every file of the declared std tree, unconditionally (trusted-std reads
    # are invisible to the scan).
    for path in walk_rue_files(std_dir_rel):
        entries.add(path)

    # Entries resolve against the manifest file's own directory, which keeps
    # them identical across checkout roots.
    manifest_dir = posixpath.dirname(normalize(args.out)) or "."
    lines = sorted(posixpath.relpath(entry, manifest_dir) for entry in entries)
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
