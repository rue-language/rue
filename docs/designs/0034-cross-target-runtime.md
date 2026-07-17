---
id: 0034
title: Per-Target Runtime Archives for Cross-Compilation
status: implemented
tags: [runtime, cross-compilation, build-system, targets]
created: 2026-06-11
accepted: 2026-06-11
implemented: 2026-07-16
spec-sections: []
superseded-by:
---

# ADR-0034: Per-Target Runtime Archives for Cross-Compilation

## Status

Implemented with option B. The compiler embeds a hermetically built runtime for
every supported target and selects and validates the matching archive for each
link. The earlier option C refusal was a temporary safety measure.

**Tracking:** RUE-36, RUE-85, RUE-862, RUE-863, RUE-864

## Problem

`rue --target aarch64-linux main.rue -o out` on an x86-64 host used to
report SUCCESS and emit an ELF whose header said AArch64 but whose entry
point contained **x86-64 machine code** (objdump-verified; the entry was
not even 4-byte aligned). The same held for `--target aarch64-macos`. The
binary was silently unrunnable on real hardware.

The aarch64 *codegen* was already sound. The broken cross-compile driver path
embedded one host-configured `rue-runtime` static library and supplied it to
every target link. The runtime contains the entry point (`_start`/`__main`),
syscall layer, panic handlers, allocation, and IO support, so this could put
real host machine code in a foreign executable.

Two partial defenses landed independently of this ADR:

1. The linker now validates each object's machine field against the link
   target and fails with `ArchMismatch` (commit `cccb160c`, RUE-131
   item 10). This stopped the *silent* part, but the message
   ("object architecture mismatch: object is X86_64, link target is
   Aarch64Linux") is misleading — the user's own objects are fine; it is
   the invisible embedded runtime that is foreign — and it names neither
   the cause nor a workaround.
2. This ADR's option C stopgap made the driver refuse foreign executable links
   until per-target archives were available.

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

Concretely, `runtime_staticlib` builds each archive with the pinned
hermetic `rustc`, target `rust-std` component, and shared runtime flags.
`crates/rue-compiler/BUCK` maps all three archives into the compiler;
`runtime_for_target()` is a total match over [`Target`](../../crates/rue-target/src/lib.rs),
and both linker modes validate the selected archive's object format, machine,
and typed runtime ABI before consuming it.

Details to handle:

- The runtime's `rustc_flags` need a per-target split:
  `-Ctarget-feature=-lse,-lse2,-outline-atomics` is aarch64-only;
  `-Crelocation-model=static`, `-Cpanic=abort`, `-Copt-level=z`, LTO
  apply everywhere.
- CI implication: every platform can build all three executable targets. The
  CLI matrix bounds and validates ELF/Mach-O headers, load structures,
  relocations, and the runtime entry architecture without executing foreign
  code; it executes the native member with string and allocation coverage.

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

- **Implemented: option B.** The build pins all three `rust-std` components,
  produces all three static libraries with the same hermetic Rust toolchain,
  embeds them, and selects by target. Option A remains disproportionate for
  one small `no_std` runtime closure; revisit if Rue gains more per-target Rust
  components.
- The compiler validates runtime presence, archive structure, object
  format/machine, and typed ABI on the canonical internal and system linker
  paths before it publishes an executable.

## Consequences

- `rue --target <supported-target> ... -o out` emits an executable for all
  three supported targets on every supported host.
- Cross-target presentation modes remain independent of linking.
- Adding a target now requires a pinned target component, runtime archive
  mapping, a total selector arm, format/machine validation, and a CI matrix
  member. A missing or malformed runtime fails before executable publication.
