#!/bin/bash

set -euo pipefail

merge_script=$1
triple=x86_64-unknown-linux-gnu
test_root=$(mktemp -d "${TMPDIR:-/tmp}/rue-merge-sysroot.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

producer="$test_root/original/producer"
rustc_dist="$producer/rustc-dist"
std_dist="$producer/std-dist"
sysroot="$producer/sysroot"

mkdir -p \
    "$rustc_dist/rustc/bin" \
    "$rustc_dist/rustc/libexec" \
    "$rustc_dist/rustc/lib/rustlib/etc" \
    "$rustc_dist/rustc/lib/rustlib/$triple/bin" \
    "$std_dist/rust-std-$triple/lib/rustlib/$triple/lib"

printf 'rustc\n' > "$rustc_dist/rustc/bin/rustc"
printf 'rustdoc\n' > "$rustc_dist/rustc/bin/rustdoc"
printf 'libexec\n' > "$rustc_dist/rustc/libexec/rust-analyzer-proc-macro-srv"
printf 'compiler dylib\n' > "$rustc_dist/rustc/lib/librustc_driver.fake"
printf 'debugger helpers\n' > "$rustc_dist/rustc/lib/rustlib/etc/gdb_load_rust_pretty_printers.py"
printf 'linker\n' > "$rustc_dist/rustc/lib/rustlib/$triple/bin/rust-lld"
printf 'standard library\n' > \
    "$std_dist/rust-std-$triple/lib/rustlib/$triple/lib/libstd.fake.rlib"

/bin/bash "$merge_script" "$rustc_dist" "$std_dist" "$sysroot" "$triple"

assert_relative_link() {
    local path=$1
    local target

    if [ ! -L "$path" ]; then
        echo "expected symlink: $path" >&2
        exit 1
    fi
    target=$(readlink "$path")
    case "$target" in
        /*)
            echo "expected relative symlink, got $path -> $target" >&2
            exit 1
            ;;
    esac
    if [ ! -e "$path" ]; then
        echo "symlink does not resolve: $path -> $target" >&2
        exit 1
    fi
}

assert_sysroot() {
    local root=$1

    assert_relative_link "$root/bin"
    assert_relative_link "$root/libexec"
    assert_relative_link "$root/lib/librustc_driver.fake"
    assert_relative_link "$root/lib/rustlib/etc"
    assert_relative_link "$root/lib/rustlib/$triple/bin"
    assert_relative_link "$root/lib/rustlib/$triple/lib"

    [ "$(cat "$root/bin/rustc")" = rustc ]
    [ "$(cat "$root/libexec/rust-analyzer-proc-macro-srv")" = libexec ]
    [ "$(cat "$root/lib/librustc_driver.fake")" = "compiler dylib" ]
    [ "$(cat "$root/lib/rustlib/etc/gdb_load_rust_pretty_printers.py")" = "debugger helpers" ]
    [ "$(cat "$root/lib/rustlib/$triple/bin/rust-lld")" = linker ]
    [ "$(cat "$root/lib/rustlib/$triple/lib/libstd.fake.rlib")" = "standard library" ]
}

assert_sysroot "$sysroot"

mkdir -p "$test_root/relocated"
mv "$producer" "$test_root/relocated/producer"
assert_sysroot "$test_root/relocated/producer/sysroot"
