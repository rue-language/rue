#!/bin/bash

set -euo pipefail

rustc_dist=$1
std_dist=$2
sysroot=$3
triple=$4

# Work with absolute paths while calculating each link, but only store the
# relative path between the link's directory and its target. Unlike `ln -r` or
# `realpath --relative-to`, this uses only shell path manipulation available in
# the Bash shipped by macOS as well as Linux.
rustc_dist=$(cd "$rustc_dist" && pwd)
std_dist=$(cd "$std_dist" && pwd)
sysroot_parent=$(cd "$(dirname "$sysroot")" && pwd)
sysroot="$sysroot_parent/$(basename "$sysroot")"

relative_path() {
    local from=${1#/}
    local to=${2#/}
    local from_head
    local to_head
    local result=

    # Remove the common path prefix one component at a time.
    while [ -n "$from" ] && [ -n "$to" ]; do
        from_head=${from%%/*}
        to_head=${to%%/*}
        if [ "$from_head" != "$to_head" ]; then
            break
        fi

        if [ "$from" = "$from_head" ]; then
            from=
        else
            from=${from#*/}
        fi
        if [ "$to" = "$to_head" ]; then
            to=
        else
            to=${to#*/}
        fi
    done

    # Walk up once for every component left in the link's directory.
    while [ -n "$from" ]; do
        result="../$result"
        if [ "$from" = "${from%%/*}" ]; then
            from=
        else
            from=${from#*/}
        fi
    done

    result="$result$to"
    if [ -z "$result" ]; then
        result=.
    fi
    printf '%s\n' "$result"
}

link_relative() {
    local target=$1
    local link=$2
    local link_dir
    local target_dir
    local target_path

    link_dir=$(cd "$(dirname "$link")" && pwd)
    target_dir=$(cd "$(dirname "$target")" && pwd)
    target_path="$target_dir/$(basename "$target")"
    ln -s "$(relative_path "$link_dir" "$target_path")" "$link"
}

# Create the sysroot structure and merge the separately packaged compiler and
# standard-library components into the layout rustc expects.
mkdir -p "$sysroot"
link_relative "$rustc_dist/rustc/bin" "$sysroot/bin"
link_relative "$rustc_dist/rustc/libexec" "$sysroot/libexec"

mkdir -p "$sysroot/lib/rustlib/$triple"

# Link compiler libraries while reserving rustlib for the merged structure.
for file in "$rustc_dist/rustc/lib/"*; do
    name=$(basename "$file")
    if [ "$name" != "rustlib" ]; then
        link_relative "$file" "$sysroot/lib/$name"
    fi
done

link_relative \
    "$rustc_dist/rustc/lib/rustlib/etc" \
    "$sysroot/lib/rustlib/etc"
link_relative \
    "$rustc_dist/rustc/lib/rustlib/$triple/bin" \
    "$sysroot/lib/rustlib/$triple/bin"
link_relative \
    "$std_dist/rust-std-$triple/lib/rustlib/$triple/lib" \
    "$sysroot/lib/rustlib/$triple/lib"
