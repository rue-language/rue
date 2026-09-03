/*
 * Linker script for every Linux link that runs through `zig cc`
 * (toolchains//:zig-cxx-tools).
 *
 * Zig compiles its runtime support from source on first use: the glibc start
 * files and non-shared stubs, libunwind (what `-lgcc_s` resolves to), and
 * compiler_rt. It compiles them with DWARF, and clang records the process's
 * working directory as each unit's DW_AT_comp_dir. That directory is the
 * checkout root of the Buck action, so a link would otherwise embed the
 * checkout path and two checkouts would produce different compilers
 * (scripts/check-reproducible-compiler.sh). Zig offers no way to strip only
 * its runtime objects: every strip request becomes LLD's `-s`, which also
 * drops the output's symbol table.
 *
 * Those objects are the only inputs that live under the Zig cache
 * directories, which the toolchain's cache wrapper (toolchains/zig/defs.bzl)
 * names `zig-local-cache` and `zig-global-cache`. Discarding the debug
 * sections of exactly those inputs leaves the debug information of every
 * other object, the Rust standard library's line tables included, and the
 * symbol table untouched.
 *
 * INSERT keeps LLD's default layout: without it a SECTIONS command replaces
 * the built-in script, and the program headers change.
 */
SECTIONS {
  /DISCARD/ : {
    *zig-global-cache/*(.debug_*)
    *zig-local-cache/*(.debug_*)
  }
}
INSERT AFTER .text;
