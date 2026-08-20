---
id: 0057
title: "File IO v0: pure-Rue fs over @syscall with normalized FileError"
status: accepted
tags: [stdlib, io, syscalls, runtime, error-handling, ownership]
feature-flag: null
created: 2026-07-16
accepted: 2026-07-16
implemented: 2026-07-16
spec-sections: []
superseded-by:
relates: ["RUE-712", "RUE-713", "RUE-935", "RUE-940", "ADR-0034", "ADR-0038"]
---

# ADR-0057: File IO v0 — pure-Rue fs over `@syscall`, normalized `FileError`

## Status

Accepted. Ratified by Steve on 2026-07-16, resolving the RUE-712 design gate.
The rulings below are settled; this ADR records and motivates them. Builds on
ADR-0034 (per-target runtime, host-only), ADR-0038 (`Result`, must-check via
linearity), and ADR-0039 (`@drop`). Sets the errno-normalization precedent that
RUE-713 (network IO) inherits.

**RUE-712 is the implementing issue** and the design gate; `std.fs` v0 lands
under it. It is one leg of the runtime-facility work: **RUE-935** is the sibling
argv/env access issue (process inputs, not files), and **RUE-940** is the
maturity-ladder milestone that consumes both RUE-712 and RUE-935 — neither is a
File IO implementation phase. **RUE-713** (network IO) is downstream and reuses
this ADR's errno-normalization and owned-handle patterns.

## Summary

`std.fs` is **pure Rue source** over the `@syscall` intrinsic, not a set of Rust
runtime helpers. Per-target syscall numbers are selected in Rue by a
`@target_arch()`/`@target_os()` match, exactly as `std.exit()` already does.
Paths are byte-string `StrBuf`s, NUL-appended at the syscall boundary. Every
fallible operation returns `Result(T, FileError)`, where `FileError` is a small
normalized enum decoded from per-OS errno tables in Rue — a raw errno never
escapes. A file is an owned `File` struct wrapping the fd, closed by its `drop
fn` on scope exit, with a consuming `close(self)` for callers that want to check
the close error. v0 ships `open`/`create`/`append`/`read`/`write`/`write_all`/
`close`.

## Context

Rue can already terminate a process through a syscall (`std.exit()`), read stdin
(ADR-0021), and allocate (ADR-0011), but it cannot open, read, or write a file.
Real programs need this before Rue is dogfoodable, and the surrounding decisions
— *where* the code lives, how paths and errors are modeled, how the fd's
lifetime is managed — do not shard into independent issues: the handle type, the
error type, and the buffer type are one design wearing three hats. RUE-712 was
opened as the gate to settle them together before any code lands.

Two prior decisions constrain the space. **ADR-0034** established that the Rue
runtime is embedded per-target and host-only: the compiler embeds exactly one
architecture's runtime archive, and adding foreign runtime code (new `__rue_*`
Rust helpers) multiplies the cross-target archive/validation surface that
ADR-0055 now polices. **ADR-0038** made `Result` the error-carrying type with
`?` propagation, but its must-check-via-linearity half (a `Result` is not yet a
`linear` type) is *not* implemented — tracked by RUE-591 — so an ignored
`Result` is today silently dropped rather than rejected.

## Decision

### 1. Strategy: pure Rue over `@syscall`, per-target dispatch in source

`std.fs` is written in Rue and issues syscalls through `@syscall`. This is the
direct consequence of ADR-0034: putting IO in the Rust runtime would add
host-only helpers that every cross-target archive must then carry and validate,
whereas Rue source compiles for every target for free and keeps the runtime
freestanding. It follows the `std.exit()` template already in `std/_std.rue`: a
private helper returns the per-target syscall number via a total
`@target_arch()` → `@target_os()` match returning integer literals, and the call
site wraps `@syscall(...)` in a `checked { ... }` block. `std/fs.rue` owns its
own per-target helpers (`_sys_openat`/`_sys_read`/`_sys_write`/`_sys_close`,
`_at_fdcwd`, the `_o_*` flag helpers, and the errno tables), each shaped exactly
like `_exit_syscall_number()`.

The syscall **numbers**, argument shapes, flag values, and errno values are
per-target data owned by `std.fs`. The tables are the honest home for the traps:

- **openat(2) is used uniformly on every target.** AArch64 Linux has *no*
  `open` syscall — the generic syscall ABI it uses provides only `openat(2)` —
  so `std.fs` issues `openat(AT_FDCWD, path, flags, mode)` everywhere rather
  than special-casing one target. This is both correct on arm64 Linux and
  uniform elsewhere; the "this target lacks that syscall" fact is stated once,
  by choosing openat as the single path. The concrete numbers used:

  | op      | x86-64 Linux | aarch64 Linux | macOS (both arches) |
  | ------- | ------------ | ------------- | ------------------- |
  | openat  | 257          | 56            | 463                 |
  | read    | 0            | 63            | 3                   |
  | write   | 1            | 64            | 4                   |
  | close   | 3            | 57            | 6                   |

  `AT_FDCWD` is **-100** on Linux and **-2** on macOS (per-OS, not shared).

### 2. Paths: NUL-appended byte strings, no `Path` type

Paths are `StrBuf` (ADR-0035/0043 byte strings — conventionally UTF-8, may hold
arbitrary bytes, which is exactly right for filesystem paths). `StrBuf` is
**not** NUL-terminated (`cap`-owned packed bytes, `len` is the byte count), so
the fs layer copies the bytes into a fresh `len + 1` buffer with a trailing NUL
at the syscall boundary and never exposes that terminator back to the caller.
The copy reads the source bytes through the public `StrBuf.byte_at(i) ->
Option(u8)` accessor (landed for RUE-712's benefit in PR #1714), so `fs` never
touches `StrBuf`'s private buffer. There is **no `Path`/`PathBuf` abstraction in
v0** — a byte string is the path. A typed path layer can arrive later without
changing the syscall boundary.

### 3. Error model: a normalized `FileError`, never a raw errno

Every fallible operation returns `Result(T, FileError)` (the ADR-0038 library
`Result`; `?` works for it). `FileError` is a payload enum:

```rue
enum FileError {
    NotFound,           // ENOENT
    PermissionDenied,   // EACCES, EPERM
    AlreadyExists,      // EEXIST
    Interrupted,        // EINTR — retryable; surfaced, not hidden
    WouldBlock,         // EAGAIN / EWOULDBLOCK
    InvalidInput,       // EINVAL, EBADF
    Other(i64),         // the raw errno, for everything not yet named
}
```

The named set is deliberately small and chosen from errno cases that are common
across both supported kernels and that programs routinely branch on; everything
else falls through to `Other(i64)` carrying the raw number, so no error is ever
lost. The normalization is done **in Rue**, from a per-OS errno table, because
**Linux and macOS assign different integer values to the same logical error**.
The low numbers agree — `EPERM=1`, `ENOENT=2`, `EINTR=4`, `EBADF=9`, `EACCES=13`,
`EEXIST=17`, `EINVAL=22` on both — but the two allocations diverge quickly, and
`std.fs` already crosses one such divergence: **`EAGAIN` is 11 on Linux and 35
on macOS**, so `WouldBlock` needs a per-OS table row. If `std.fs` returned the
raw errno as an `i64`, a program that wrote `match e { 2 => ... }` would silently
mean different things on different targets — a portability trap of exactly the
kind Rue exists to eliminate. Normalizing to a target-independent enum makes
error-matching programs mean the same thing everywhere.

This is the **precedent RUE-713 (network IO) inherits**: network errors will be
normalized from per-OS errno tables into a sibling enum the same way, rather than
leaking raw errnos.

#### 3a. macOS error-detection gap (discovered during implementation)

Error *detection* here relies on a failed syscall returning a **negative** value
(`-errno`). Linux does exactly that. **macOS/Darwin does not**: on failure it
returns the **positive** errno in `x0` and signals failure with the **carry
flag** — and the current `@syscall` codegen lowering (aarch64 and x86-64)
**discards the carry flag**, moving only `x0` into the result. Empirically, on
aarch64-macos a failed `openat` of a missing path returns `2` (positive ENOENT),
indistinguishable from a successful `openat` that returned fd 2. The Rust
runtime's own syscall wrappers (`crates/rue-runtime/src/aarch64_macos.rs`)
already handle this correctly — they `cset err, cs` and negate on carry — but
that logic lives in hand-written asm, not in the `@syscall` intrinsic.

This could not be fixed inside `std.fs` (the carry flag is not observable from
Rue); it required a **codegen change to the `@syscall` lowering** (negate-on-
carry on Darwin, mirroring the runtime wrappers). That fix was filed as RUE-945
and **landed while this ADR was in flight** (PR #1720): `@syscall` now presents
the uniform `-errno` convention on every target, the CLI error-detection cases
run un-gated on all platforms, and this section is retained as the historical
record of why the intrinsic's contract is what it is.

### 4. Handle: an owned `File` with drop-close and a consuming `close`

```rue
struct File {
    fd: i32,

    drop fn(inout self) { ... }                      // closes fd on scope exit
    fn close(self) -> Result((), FileError) { ... }  // consuming, checkable
}
```

`File` owns its fd. Its `drop fn` closes the fd when the value leaves scope, so
the common case needs no explicit close and cannot leak the descriptor. Callers
who must observe close failures (a write-back filesystem can report errors only
at `close`) call the **consuming** `close(self) -> Result((), FileError)`.

**Double-close guard.** `close(self)` takes `File` by value, so the caller's
binding is moved out and its scope-end drop does not fire on the original. But
`close`'s own `self` is a live `File` whose drop glue would run at the end of
`close` — a second close of the same fd (verified: a by-value consumed value
does run its destructor at the consuming function's scope exit). The guard is a
**sentinel fd** (`-1`): `close` reads the fd, writes the sentinel back, issues
the raw close syscall on the saved fd, and returns; when `self`'s drop glue then
runs, the `drop fn` sees `fd == -1` and no-ops. Because a by-value `self` is
**immutable** in Rue (there is no `mut self` receiver, and assigning to `self`
is E0203), the sentinel is written by rebinding `let mut me = self; me.fd = -1;`
— legal in a normal consuming method (unlike inside a `drop fn`, where moving
`self` out is E0442). The `drop fn` uses the same `fd >= 0` check, so a sentinel
value is never closed. This is the mechanism because ADR-0039 deferred
`@forget`/`@leak`: without a way to consume a value *without* running its
destructor, the destructor must be made idempotent via the sentinel rather than
suppressed.

**Known accepted wart.** Because ADR-0038's linearity half is not yet
implemented (RUE-591), `Result` is not `linear`, so a caller who writes
`file.close();` and ignores the returned `Result` has that error **silently
discarded** — the must-check guarantee that would force a `match`/`?`/`@drop`
does not yet fire. This is accepted for v0 and is exactly the hole ADR-0038 +
RUE-591 will close; no `std.fs`-specific machinery is added to compensate.

### 5. v0 operation scope and the buffer seam

**In v0:** open via three flag-free constructors — `File.open(path)`
(`O_RDONLY`), `File.create(path)` (`O_WRONLY|O_CREAT|O_TRUNC`, mode `0o644`),
and `File.append(path)` (`O_WRONLY|O_CREAT|O_APPEND`, mode `0o644`) — plus
`read`, `write`, `write_all`, and `close`. Separate constructors avoid a
flags-enum design in v0. The open-flag *values* are per-OS data (another table
row), because Linux and macOS disagree: `O_CREAT` is `0o100`=64 on Linux but
`0x0200`=512 on macOS, `O_TRUNC` is `0o1000`=512 vs `0x0400`=1024, and
`O_APPEND` is `0o2000`=1024 vs `0x0008`=8 (`O_RDONLY`/`O_WRONLY` agree at 0/1).

**Explicitly deferred:** seek, metadata/`stat`, directory operations,
`rename`/`unlink`, buffering layers, and any stdin/stdout/file unification.

**Buffers ship on `ArrayBuf(u8)` with raw-pointer marshalling**, because slices
(`[u8]`) are available now that ADR-0043's slice rung is stabilized. `read`
appends up to the buffer's **spare capacity** (`capacity() - len()`) and
`write`/`write_all` send the buffer's bytes.

**Read semantics: `Ok(0)` is EOF only; a full buffer is `InvalidInput`.** A
subtle footgun surfaced in pre-merge dogfooding: if `read` returned `Ok(0)` both
at real end-of-file *and* when the buffer had zero spare capacity, the natural
whole-file loop (`loop { n = read(inout buf); if n == 0 { break } }`) would
silently **truncate** at the buffer's initial capacity — a 20-byte file read
into a `with_capacity(8)` buffer would report EOF after 8 bytes. v0 closes this:
`read` on a buffer with no spare capacity (`capacity() == len()`) returns
`Err(FileError.InvalidInput)`, **never `Ok(0)`**, so `Ok(0)` unambiguously means
EOF. To make the whole-file loop writable, `ArrayBuf(u8)` exposes a public
`reserve(inout self, additional: u64)` (the parameterized capacity hint, mirroring
`StrBuf.reserve`): the loop becomes `loop { buf.reserve(chunk); n =
read(inout buf)?; if n == 0 { break } }`, growing the buffer each pass so `read`
always has spare room and only ever returns `Ok(0)` at true EOF. Loud beats
silent. Because `ArrayBuf`'s backing pointer is module-private and there
is no cross-module accessor yet, v0 marshals through a **temporary contiguous
`@alloc_bytes` buffer**, copying bytes in/out via the public `push`/`get_or`
API, and issues the syscall on that temporary. This is a **known seam, recorded
with migration intent**: now that slices are stable, the read/write signatures
can migrate to `borrow [u8]` / `inout [u8]` and the temporary-copy can disappear —
a signature change at one boundary, not a redesign. Committing to `ArrayBuf(u8)`
unblocked IO without waiting for the slice feature.

`std.fs` source is trusted standard-library input (`ModuleOrigin::
StandardLibrary`), so its use of the `raw_bytes` packed-byte intrinsics
(`@alloc`/`@free`/`@ptr_read`/`@ptr_write`/`@ptr_offset`/`@ptr_to_int`, the
unified surface ADR-0059 folded the original `_bytes` and `@byte_read`/
`@byte_write` names into) is authorized without a preview flag, exactly as
`std/strbuf.rue` is — programs that consume `std.fs` need no
`--preview raw_bytes`.

## Implementation status

Implemented under RUE-712 on 2026-07-16:

- `std/fs.rue` (new): the `FileError` enum, per-target syscall/flag/errno tables,
  path marshalling, and the `File` handle with all v0 operations.
- `std/_std.rue`: `pub const fs = @import("fs.rue");` re-export.
- CLI coverage (`crates/rue-cli-tests/cases/fs_file_io.toml`): create+write+
  read-back round-trip, append semantics, drop-close + reopen, consuming-close +
  double-close safety (all run on every target); open-missing → `NotFound` and
  write-to-read-only → `InvalidInput` (gated `only_on` Linux per §3a).

## Consequences

### Positive
- No new runtime helpers: `std.fs` compiles for every target for free and keeps
  the runtime host-only per ADR-0034; nothing added to the ADR-0055 archive
  surface.
- Error-matching programs are target-independent — the normalized enum means the
  same thing on every OS, unlike a raw errno.
- The fd cannot leak (drop-close) yet close errors remain observable (consuming
  `close`).
- Reuses existing machinery end to end: the `std.exit()` dispatch template, the
  ADR-0038 `Result`/`?`, `StrBuf`/`ArrayBuf`, `StrBuf.byte_at`, and `checked`
  blocks.

### Negative
- Syscall-number, open-flag, and errno tables are hand-maintained per target; a
  new target adds rows to all three.
- Correct error detection depends on the `@syscall` lowering normalizing
  Darwin's carry flag into the negative-errno convention (§3a). That landed in
  RUE-945, so the tables here are read the same way on every target.
- Until RUE-591, an ignored `close()` `Result` is silently dropped.
- v0 is minimal: no seek/stat/dirs/rename/buffering.
- The buffer API is on `ArrayBuf(u8)` and pays a temporary-copy per read/write;
  both the signature and the copy can change now that slices are stable.

### Neutral
- No language-semantics or preview-feature change; this is stdlib + intrinsic use.
- `Path` typing, buffered IO, and stream unification are left open by design.

## Alternatives Considered

- **Return the raw errno as `i64`.** Rejected: Linux and macOS give the same
  logical error different integer values (e.g. `EAGAIN` 11 vs 35), so
  `match e { N => … }` would be silently target-dependent — the portability trap
  normalization exists to prevent. `Other(i64)` still carries the raw value for
  the unnamed long tail.
- **A raw fd (`i32`) handle instead of an owned `File`.** Rejected: a bare fd
  models nothing — it does not close on scope exit (descriptor leaks) and gives
  the type system nothing to enforce. The owned `File` with drop-close is the
  affine-ownership payoff.
- **Runtime `__rue_*` IO helpers in Rust.** Rejected by ADR-0034: the runtime is
  host-only and embedded per target, so Rust IO helpers would multiply the
  cross-target archive and ADR-0055 validation surface. Pure Rue over `@syscall`
  avoids all of it. (The Darwin carry-flag handling those helpers already do is
  what §3a wants lifted into the `@syscall` lowering — a small, contained
  codegen change, not a return to runtime IO.)
- **Special-casing `open` vs `openat` per target.** Rejected in favor of
  uniform `openat(AT_FDCWD, …)`: AArch64 Linux has no `open` at all, and openat
  is available and equivalent everywhere, so one path is simpler and correct.
- **A `Path`/`PathBuf` type in v0.** Deferred: a byte-string path needs no new
  type, and the syscall boundary is unaffected by adding typed paths later.

## Future Work

- **Teach the `@syscall` lowering to negate-on-carry on Darwin** so macOS error
  detection works and the Linux-gated error tests can run on every target (§3a).
- Seek, `stat`/metadata, directory iteration, `rename`/`unlink`.
- Buffered IO and stdin/stdout/file unification.
- Migrate read/write buffers from `ArrayBuf(u8)` to `borrow [u8]`/`inout [u8]`
  now that slices are stable, dropping the temporary-copy marshalling.
- Close the must-check hole once ADR-0038's linearity half (RUE-591) lands.
- RUE-713 network IO, reusing this errno-normalization precedent.
