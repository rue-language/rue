#!/usr/bin/env bash
# Generate rust-project.json from Buck2 targets for rust-analyzer support
#
# Usage: ./gen-rust-project.sh
#
# This queries Buck2 for all Rust targets and generates a rust-project.json
# file that rust-analyzer can use for IDE support.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "Querying Buck2 for Rust targets..."
./buck2 targets //crates/... --json 2>/dev/null > /tmp/buck_targets.json

echo "Generating rust-project.json..."
python3 << 'EOF'
import json
import os

with open('/tmp/buck_targets.json') as f:
    targets = json.load(f)

root = os.getcwd()

# Build crate index
crates = []
crate_name_to_idx = {}

for target in targets:
    ttype = target.get('buck.type', '')
    name = target.get('name', '')

    if 'rust_library' in ttype or 'rust_binary' in ttype:
        if not name.endswith('-test'):
            crate_name_to_idx[name] = len(crates)
            crates.append(target)

rust_project = {
    "sysroot_src": None,
    "crates": []
}

for idx, target in enumerate(crates):
    name = target.get('name', '')
    srcs = target.get('srcs', [])
    deps = target.get('deps', [])

    # Find crate root
    root_file = None
    for src in srcs:
        src_path = src.replace('root//', '')
        if src_path.endswith('lib.rs') or src_path.endswith('main.rs'):
            root_file = src_path
            break

    if not root_file and srcs:
        root_file = srcs[0].replace('root//', '')

    if not root_file:
        continue

    # Convert deps to indices
    dep_indices = []
    for dep in deps:
        if dep.startswith('root//'):
            dep_name = dep.split(':')[-1]
            if dep_name in crate_name_to_idx:
                dep_indices.append({"crate": crate_name_to_idx[dep_name], "name": dep_name.replace('-', '_')})

    # Use relative paths - rust-analyzer resolves them relative to rust-project.json
    rust_project["crates"].append({
        "display_name": name,
        "root_module": root_file,
        "edition": "2024",
        "deps": dep_indices,
        "is_workspace_member": True,
        "source": {
            "include_dirs": [os.path.dirname(root_file)],
            "exclude_dirs": []
        },
        "cfg": [],
        "env": {},
        "is_proc_macro": False,
    })

with open(os.path.join(root, 'rust-project.json'), 'w') as f:
    json.dump(rust_project, f, indent=2)
    f.write('\n')

print(f"Generated rust-project.json with {len(rust_project['crates'])} crates")
EOF

echo "Done! rust-analyzer should now work with this project."
echo "You may need to restart rust-analyzer or reload your IDE."
