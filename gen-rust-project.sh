#!/usr/bin/env bash
# Generate rust-project.json from Buck2 targets for rust-analyzer support.
#
# Usage: ./gen-rust-project.sh
#
# Rue has no Cargo workspace, so rust-project.json IS the IDE project model, not
# an optional fallback (RUE-516). This queries Buck2 for the first-party crates
# AND every third-party crate they actually depend on (following Buck `alias` /
# `transition_alias` / `rust_proc_macro_alias` indirection), so rust-analyzer
# resolves both `crates/**` and vendored dependencies like `lasso`.
#
# Modeled: first-party + used third-party rust_library/rust_binary crates, their
# dep edges, editions, feature cfgs, and proc-macro flags. Deliberately NOT
# modeled yet (rust-analyzer degrades gracefully): the sysroot source
# (`sysroot_src` is null; rust-analyzer falls back to `rustc --print sysroot`)
# and proc-macro dylib paths (macros resolve as crates but are not expanded).
# The model targets the default host platform. After regenerating, run
# `python3 scripts/validate-rust-project.py` (also a CI gate).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

targets_json="$(mktemp)"
gen_err="$(mktemp)"
trap 'rm -f "$targets_json" "$gen_err"' EXIT

echo "Querying Buck2 for Rust targets (first-party + third-party)..."
# Guard the query status and preserve its stderr on failure: a swallowed Buck
# error used to surface downstream as an unrelated Python traceback on an
# empty/partial JSON file (RUE-550).
gen_status=0
./buck2 targets //crates/... //third-party/... --json 2>"$gen_err" > "$targets_json" \
    || gen_status=$?
if [ "$gen_status" -ne 0 ]; then
    echo "gen-rust-project.sh: 'buck2 targets //crates/... //third-party/...' failed (exit $gen_status):" >&2
    cat "$gen_err" >&2
    exit "$gen_status"
fi

echo "Generating rust-project.json..."
python3 - "$targets_json" << 'EOF'
import json
import os
import sys

with open(sys.argv[1]) as f:
    targets = json.load(f)

# Index every target by its full label so we can resolve dep/alias references.
by_label = {}
for t in targets:
    label = f"{t.get('buck.package', '')}:{t.get('name', '')}"
    by_label[label] = t


def kind(t):
    # buck.type looks like "prelude//rules.bzl:rust_library".
    return t.get("buck.type", "").split(":")[-1]


ALIAS_KINDS = {"alias", "transition_alias", "configured_alias"}


def is_build_script(name):
    return "build-script" in name


def to_repo_path(t, p):
    """Resolve a target src/crate_root to a repo-root-relative path. Buck gives
    these either as full `root//...` labels or relative to the target's package
    (third-party `crate_root`), so handle both."""
    if p.startswith("root//"):
        return p[len("root//"):]
    pkg = t.get("buck.package", "").replace("root//", "")
    return f"{pkg}/{p}" if pkg else p


def resolve_concrete(label, seen=None):
    """Follow alias chains to the concrete target label, or None.

    Proc-macro deps go through a `rust_proc_macro_alias` (which points at the
    real library via `actual_plugin`, not `actual`), usually behind a plain
    `alias` first: e.g. `:logos-derive` (alias) -> `:logos-derive-0.14.4`
    (proc_macro_alias) -> `:_logos-derive-0.14.4` (the real rust_library)."""
    seen = seen or set()
    if label in seen:
        return None
    seen.add(label)
    t = by_label.get(label)
    if t is None:
        return None
    k = kind(t)
    if k == "rust_proc_macro_alias":
        nxt = t.get("actual_plugin") or t.get("actual_exec")
        return resolve_concrete(nxt, seen) if nxt else None
    if k in ALIAS_KINDS:
        actual = t.get("actual")
        return resolve_concrete(actual, seen) if actual else None
    return label


def is_crate(t):
    if kind(t) not in ("rust_library", "rust_binary"):
        return False
    return not is_build_script(t.get("name", ""))


def is_first_party(label):
    return label.startswith("root//crates/")


# Closure: start from first-party crates, walk deps (resolving aliases), and
# collect every concrete crate reached. This naturally pulls in the third-party
# crates actually used and skips unused vendored packages and build scripts.
wanted = {}
queue = [lbl for lbl, t in by_label.items() if is_first_party(lbl) and is_crate(t)]
while queue:
    concrete = resolve_concrete(queue.pop())
    if concrete is None:
        continue
    t = by_label.get(concrete)
    if t is None or not is_crate(t) or concrete in wanted:
        continue
    wanted[concrete] = t
    queue.extend(t.get("deps", []))


def crate_name(t):
    # The name used in `use <crate>` — the `crate` attribute (e.g. lasso-0.7.3 →
    # "lasso"), falling back to the sanitized target name.
    return t.get("crate") or t.get("name", "").replace("-", "_")


def root_module(t):
    cr = t.get("crate_root")
    if cr:
        return to_repo_path(t, cr)
    lib = main = first = None
    for s in t.get("srcs", []):
        sp = to_repo_path(t, s)
        if sp.endswith("lib.rs") and lib is None:
            lib = sp
        elif sp.endswith("main.rs") and main is None:
            main = sp
        elif first is None:
            first = sp
    return lib or main or first


# Deterministic ordering so the checked-in file is stable across runs.
labels = sorted(wanted)
index = {lbl: i for i, lbl in enumerate(labels)}

crates = []
for lbl in labels:
    t = wanted[lbl]
    rm = root_module(t)
    if not rm:
        continue

    dep_entries = []
    seen = set()
    for d in t.get("deps", []):
        c = resolve_concrete(d)
        if c is None or c == lbl or c not in index or c in seen:
            continue
        seen.add(c)
        dep_entries.append({"crate": index[c], "name": crate_name(wanted[c])})

    cfg = ["test", "debug_assertions"]
    cfg += [f'feature="{feat}"' for feat in t.get("features", [])]

    crates.append({
        "display_name": t.get("name", ""),
        "root_module": rm,
        "edition": t.get("edition") or "2024",
        "deps": sorted(dep_entries, key=lambda d: d["crate"]),
        "is_workspace_member": is_first_party(lbl),
        "source": {"include_dirs": [os.path.dirname(rm)], "exclude_dirs": []},
        "cfg": cfg,
        "env": {},
        "is_proc_macro": bool(t.get("proc_macro")),
    })

# Some first-party crates live outside the Buck graph as standalone Cargo crates
# (e.g. rue-runtime-asan, the sanitizer harness — RUE-560). Buck cannot see them,
# but a contributor still edits them, so add a minimal entry for any crates/<dir>
# with a lib.rs/main.rs that the Buck closure did not already cover. They carry no
# resolved dep edges, but their source is at least indexed.
covered_dirs = set()
for c in crates:
    parts = c["root_module"].split("/")
    if len(parts) >= 2 and parts[0] == "crates":
        covered_dirs.add(parts[1])

for entry in sorted(os.listdir("crates")):
    directory = os.path.join("crates", entry)
    if not os.path.isdir(directory) or entry in covered_dirs:
        continue
    rm = next(
        (f"crates/{entry}/{cand}" for cand in ("src/lib.rs", "src/main.rs")
         if os.path.exists(os.path.join(directory, cand))),
        None,
    )
    if rm is None:
        continue
    edition = "2024"
    cargo = os.path.join(directory, "Cargo.toml")
    if os.path.exists(cargo):
        for line in open(cargo):
            stripped = line.strip()
            if stripped.startswith("edition"):
                edition = stripped.split("=", 1)[1].strip().strip('"') or edition
                break
    crates.append({
        "display_name": entry,
        "root_module": rm,
        "edition": edition,
        "deps": [],
        "is_workspace_member": True,
        "source": {"include_dirs": [os.path.dirname(rm)], "exclude_dirs": []},
        "cfg": ["test", "debug_assertions"],
        "env": {},
        "is_proc_macro": False,
    })

rust_project = {"sysroot_src": None, "crates": crates}
with open("rust-project.json", "w") as f:
    json.dump(rust_project, f, indent=2)
    f.write("\n")

first_party = sum(1 for c in crates if c["is_workspace_member"])
print(
    f"Generated rust-project.json: {len(crates)} crates "
    f"({first_party} first-party, {len(crates) - first_party} third-party)"
)
EOF

echo "Done. Validate with: python3 scripts/validate-rust-project.py"
echo "You may need to restart rust-analyzer or reload your IDE."
