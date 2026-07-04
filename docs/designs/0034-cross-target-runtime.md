---
id: 0034
title: Per-Target Runtime Archives for Cross-Compilation
status: accepted
tags: [runtime, cross-compilation, build-system, targets]
created: 2026-06-11
accepted: 2026-06-11
implemented:
spec-sections: []
superseded-by:
---

# ADR-0034: Per-Target Runtime Archives for Cross-Compilation

## Status

Accepted. Option B was ratified for the current implementation; option A is the
probable eventual destination. The stopgap refusal behavior shipped in PR #978.

**Tracking:** RUE-36, RUE-144 item 7, RUE-85

## Problem

`rue --target aarch64-linux main.rue -o out` on an x86-64 host used to
report SUCCESS and emit an ELF whose header said AArch64 but whose entry
point contained **x86-64 machine code** (objdump-verified; the entry was
not even 4-byte aligned). The same held for `--target aarch64-macos`. The
binary was silently unrunnable on real hardware.

The aarch64 *codegen* is fine — CI builds `rue` natively on arm64 and
runs the produced binaries. Only the cross-compile **driver** path was
broken, and the root cause is in the build graph, not in the compiler
source:

- `crates/rue-compiler/BUCK` maps `//crates/rue-runtime:rue-runtime[staticlib]`
  into `src/librue_runtime.a`, which `lib.rs` embeds via `include_bytes!`.
- Buck2 builds that staticlib in the **host configuration**. There is
  exactly one embedded archive, and it is always the host's.
- Every link (internal ELF/Mach-O linker or `--linker <system>`) pulled
  that host archive into the output regardless of `--target`. The
  runtime contains the entry point (`_start`/`__main`), the syscall
  layer, panic handlers, and all `String`/IO support — i.e. real
  executable code of the wrong architecture.

Two partial defenses landed independently of this ADR:

1. The linker now validates each object's machine field against the link
   target and fails with `ArchMismatch` (commit `cccb160c`, RUE-131
   item 10). This stopped the *silent* part, but the message
   ("object architecture mismatch: object is X86_64, link target is
   Aarch64Linux") is misleading — the user's own objects are fine; it is
   the invisible embedded runtime that is foreign — and it names neither
   the cause nor a workaround.
2. This ADR's stopgap (option C below, implemented): the driver refuses
   the link up front with an error that explains exactly what is
   missing.

## Goals

- A `--target` link either produces a **correct-architecture, runnable**
  executable or fails with an error that names the limitation.
- Cross-target code generation (`--emit asm/mir/...`) keeps working on
  every host for every target.
- Hermeticity: the runtime used for target X must be bit-identical no
  matter which host compiled it (same rustc version, same flags).

## Options Considered

### A. Per-target staticlibs via Buck2 target platforms / transitions

Declare Buck2 `platform()`s for the three Rue targets, apply a
configuration transition (or `configured_alias`) to build
`rue-runtime[staticlib]` once per target, map all three archives into
`rue-compiler`, and have the driver select by `Target`.

- Pros: idiomatic Buck2; the runtime build stays a normal `rust_library`
  so flags/deps are declared once.
- Cons: the heaviest option. The vendored hermetic toolchain
  (`toolchains/rust/defs.bzl`) is **host-only**: each Rust dist tarball
  ships `rust-std` for its own triple only (verified in `buck-out`: the
  x86-64 Linux dist contains only `rust-std-x86_64-unknown-linux-gnu`).
  Cross-configuration `rust_library` builds additionally require
  per-platform toolchain registration, exec-platform plumbing, and
  prelude cooperation. High risk of fighting the prelude for what is, in
  the end, one no-std crate.

### B. Per-target staticlib build rules using the hermetic rustc directly

`rue-runtime` is `#![no_std]`, has **zero dependencies**, and needs only
`core`/`compiler_builtins`. Compiling it for a foreign target needs just:

1. The already-vendored host `rustc` invoked with `--target <triple>`.
2. The target's std component: pin three extra
   `rust-std-1.92.0-{triple}.tar.xz` archives (same `http_archive` +
   sha256 pattern as `RUST_RELEASES`) and extend the merged sysroot —
   or pass `-L` to the component's `lib/rustlib/{triple}/lib`.
3. No platform linker at all: `--crate-type staticlib` only archives
   objects (rustc's internal ar), so a Linux host can build the
   aarch64-macos runtime archive and vice versa.

Concretely: a small `cross_runtime` rule (or genrule) per target that
runs `rustc --target {triple} --crate-type staticlib` with the existing
runtime flags, three `mapped_srcs` entries in `crates/rue-compiler/BUCK`
(`src/librue_runtime-{target}.a`), three `include_bytes!` statics, and
`runtime_for_target()` becomes a total match instead of a host check.

Details to handle:

- The runtime's `rustc_flags` need a per-target split:
  `-Ctarget-feature=-lse,-lse2,-outline-atomics` is aarch64-only;
  `-Crelocation-model=static`, `-Cpanic=abort`, `-Copt-level=z`, LTO
  apply everywhere.
- CI implication: every platform's build downloads the three (two
  foreign) `rust-std` components, ~40 MB compressed each, cached by
  Buck2 like the existing toolchain archives. `validate_runtime()`
  extends to all three archives, and the existing CLI cases flip from
  "refused" to "succeeds + correct ELF machine field" (each scoped
  `only_on` the hosts where the target is foreign).
- aarch64-macos output remains gated on RUE-85 (Mach-O cross-linking
  has its own issues beyond the runtime archive); the runtime archive
  part is still worth shipping so RUE-85 is the only remainder.

- Pros: smallest hermetic full fix; no platform/transition machinery;
  reuses the `http_archive` pinning pattern; trivially testable
  (`readelf -h` machine field per archive member).
- Cons: bypasses the prelude's `rust_library` for the cross builds, so
  the flag list is duplicated between the `rust_library` (host, used by
  `rue-runtime-test`) and the cross rule — must be kept in sync.

### C. Hard error: refuse to link for a target whose runtime we don't have

At link time (both internal and system linker paths), if
`options.target != Target::host()`, fail with:

```
error: [E1000]: link error: cannot link an executable for aarch64-linux:
this rue compiler was built for x86-64-linux and only embeds the
x86-64-linux runtime library, so the result would not run on
aarch64-linux (RUE-36). Cross-target code generation still works: use
`--emit asm` to inspect aarch64-linux assembly.
```

`--emit` paths never reach the linker, so cross-target codegen
inspection is untouched.

- Pros: tiny, honest, immediately ships; converts a silent (later:
  cryptic) failure into an actionable one.
- Cons: not a fix — cross-compilation simply doesn't exist until A or B
  lands.

## Decision

- **Now (this change): option C.** The check lives in
  `runtime_for_target()` in `crates/rue-compiler/src/lib.rs`, called by
  both `link_internal_with_warnings` and `link_system_with_warnings`,
  so every link path is covered and every `--emit` path is not.
- **Full fix (proposed): option B.** Pin the three `rust-std`
  components, add per-target staticlib rules invoking the hermetic
  rustc, embed all three archives, and make `runtime_for_target()` a
  total match. Option A is rejected as disproportionate machinery for
  one dependency-free no-std crate; revisit if Rue ever grows more
  per-target Rust components.

## Consequences

- Cross-target `-o` builds fail fast with a self-explanatory error
  instead of producing broken binaries (pre-`cccb160c`) or a misleading
  `ArchMismatch` (post-`cccb160c`).
- CLI suite gained a host-conditional `only_on` case scope, since
  whether `--target X` is a cross-compile depends on the host.
- When option B lands: flip the three `cross_link_to_*_refused` CLI
  cases to success cases asserting the ELF `e_machine` field (and keep
  aarch64-macos xfailed under RUE-85 until Mach-O cross-linking works),
  delete the refusal branch from `runtime_for_target()`, and mark this
  ADR Implemented.
