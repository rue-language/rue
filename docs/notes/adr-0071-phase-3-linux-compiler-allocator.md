# ADR-0071 Phase 3: Linux compiler allocator and hermetic Zig policy

This note records the bounded compiler-host change selected from the Phase 3
allocation profile. It does not claim the 500 ms / 256 MiB milestone: the next
reference scaling collection remains the authority for that milestone.

## Scope and versions

The production Rue compiler uses `mimalloc` 0.1.52 with
`libmimalloc-sys` 0.1.49 and that crate's `v2` source tree, which is mimalloc
2.3.2 (`MI_MALLOC_VERSION` 20302). The allocator applies only to the compiler
process on Linux x86-64 and Linux AArch64. macOS and any unsupported compiler
host use Rust's `System` allocator. Target Rue programs retain their own runtime
allocator and ABI; this change does not alter emitted programs.

The production selection is structural. The Rust and native dependencies occur
only in the Linux x86-64/AArch64 arms of the compiler target's nested Buck
selection. The separately built `rue-benchmark` compiler remains a
`CountingAllocator` over `System` and does not link any `mi_*` symbol, so its
allocation counts remain comparable with the preceding series.

## Native build policy

Buck compiles the bundled mimalloc 2.3.2 `static.c` directly through a pinned
Zig 0.16.0 distribution. The official x86-64 and AArch64 Linux and macOS host
archives are SHA-256 pinned `http_archive` inputs. The complete relocatable Zig
tree is hidden in its `RunInfo`; the compiler, archive implementation, libc
descriptions, and other files Zig reads therefore participate in action keys
and remote-execution inputs. Buck never invokes the repository's DotSlash
launcher from an action.

The reusable C archive rule defaults to `-g0` before caller flags, excluding
debug metadata and its source/checkout paths unless a future caller deliberately
opts in with a later debug flag. Include-directory arguments use Buck-package
semantics: the rule resolves them through the declaring label's cell-aware path
before running its action from the repository root. The mimalloc action
therefore receives the exact
`third-party/vendor/libmimalloc-sys-0.1.49/...` roots rather than unresolved
`vendor/...` paths. The mimalloc action also states permanent `-g0` explicitly
and always uses `-O3`, including debug Rust builds. The resulting native object
contains neither debug information nor checkout or source paths carried by
debug metadata. The action also fixes the Linux target, glibc 2.17 compatibility
floor, baseline CPU, static/PIC mode, initial-exec TLS, and release macros.
`__DATE__` and `__TIME__` are fixed to `Jan 01 1970` and `00:00:00`; no ambient
build timestamp enters the archive. The allocator override and secure features
are not enabled. Zig's local and global caches live under each action's private
Buck scratch directory rather than declared outputs or shared host state. Rue's
existing in-process linker remains the compiler's program linker; the Zig
toolchain is a reusable native-build foundation, not a peer Rue link path.

## Runtime settings and measurements

Production uses mimalloc's default settings. In particular, this change does
not force immediate purge: a diagnostic `MIMALLOC_PURGE_DELAY=0` run reduced
resident memory but gave back a material part of the speed improvement. The
`MIMALLOC_*` namespace is not a supported Rue compiler command-line or
configuration surface. Developers may use upstream variables for diagnostics,
but their behavior is allocator-version-specific and may change without Rue
compatibility guarantees. Reference compiler processes remove every inherited
`MIMALLOC_*` variable, matching the ASCII prefix case-insensitively, before
spawn because mimalloc consumes those variables before Rue's `main` begins.

On the hosted Linux diagnostic, six alternating one-worker Lattice rounds moved
median compiler-root time from 2,603.305 ms with `System` to 1,756.349 ms with
the optimized static mimalloc build: -846.956 ms, or -32.53%. Median peak RSS
was neutral within measurement resolution (292.47 MiB to 292.56 MiB), and all
native outputs remained byte-identical. This establishes a material bounded
improvement, not attainment of ADR-0071's Phase 3 vertical milestone.

## Provenance, licensing, updates, and rollback

Zig archive URLs and hashes come from Zig's official 0.16.0 release index.
Zig and mimalloc are MIT-licensed; their notices remain in the pinned Zig
distribution and the unpruned Reindeer vendor tree. Rue's repository continues
to carry its root MIT notice, while the vendored mimalloc notice preserves the
upstream Microsoft/Daan Leijen copyright required for redistribution.

A Zig or mimalloc update is a reviewed native-toolchain change: update the
official hash pins and exact Cargo versions together, regenerate the complete
vendor and Buck graph, confirm `MI_MALLOC_VERSION`, inspect actual C action
arguments, reproduce the compiler twice without action-cache reuse, verify
Linux x86-64 and AArch64 production symbols, verify zero `mi_*` symbols in
`rue-benchmark`, and rerun the hosted scaling comparison with exact output
hashes. Security advisories apply to both the compiler-host allocator and the
native compiler toolchain and should be triaged as compiler distribution
dependencies. Rollback is intentionally narrow: remove the Linux dependency
selection and allocator module to return the compiler to `System`; no target
runtime format or cache migration is involved.
