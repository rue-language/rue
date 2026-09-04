# Rue compiler/runtime ABI

Rue emits machine code directly and links it with a target-matching
`rue-runtime` archive. The compiler and runtime share one typed contract from
`crates/rue-runtime-abi`; this document explains the architecture and the
target rules without duplicating that contract's helper table.

## Canonical contract

`rue-runtime-abi` is the source of truth for:

- every compiler-callable runtime helper and its exported symbol;
- ordered C-boundary parameters, pointer modes, scalar results, and explicit
  result-storage aggregates;
- safety requirements, return behavior, target availability, and calling
  convention;
- separately classified entry points, compiler-built memory routines,
  platform shims, and the retained ABI-version marker; and
- the current lockstep ABI version.

Compiler phases carry `RuntimeHelperId` and typed runtime-call adaptations.
AIR and CFG do not discover runtime behavior from symbol strings. Shared call
planning validates the manifest signature before the x86-64 or AArch64 backend
assigns physical registers and stack locations. A helper symbol is materialized
only at presentation and MIR relocation boundaries.

The runtime's exported wrappers and compile-time Rust function-type assertions
are generated from the same manifest rows. The runtime implementation remains
free to organize private functions normally; only the generated C wrappers own
the externally visible helper definitions.

The exact current inventory is therefore read through:

- `RuntimeHelperId::ALL` and `RuntimeHelperId::helper()` for callable helpers;
- `ReservedExportId::ALL` and `ReservedExportId::export()` for other intentional
  runtime exports; and
- `RUNTIME_ABI_VERSION` / `RUNTIME_ABI_VERSION_SYMBOL` for version metadata.

Handwritten code and documentation must not maintain a peer signature or
export table.

## Runtime ABI versus native Rue ABI

This document describes only the typed target-C boundary from generated Rue
code to `rue-runtime`. Calls between Rue functions use a separate native Rue
convention that is intentionally not C-compatible or stable across compiler
revisions. Rue does not yet expose general foreign imports or exports.

The [FFI ABI conformance audit](notes/ffi-abi-conformance-audit.md) records the
current native convention, compares both boundaries with System V AMD64,
AAPCS64, and Apple's arm64 amendments, and defines the executable evidence
required before the target-C subset is expanded.

## Target C calling conventions

All callable runtime helpers use the target C convention declared by the
manifest.

### x86-64 Linux

The System V AMD64 convention supplies integer and pointer arguments in `rdi`,
`rsi`, `rdx`, `rcx`, `r8`, and `r9`, then on the stack. Scalar results use
`rax`. The stack is 16-byte aligned at a call boundary. `rbx`, `rbp`, and
`r12`-`r15` are callee-saved; other ordinary argument/result registers are
caller-saved.

Linux syscalls are a separate runtime-internal boundary: their number is in
`rax`, arguments use `rdi`, `rsi`, `rdx`, `r10`, `r8`, and `r9`, and the
instruction clobbers `rcx` and `r11`.

### AArch64 Linux and macOS

The AArch64 procedure-call standard supplies the first eight integer and
pointer arguments in `x0`-`x7` (or the corresponding `w` register for a
32-bit scalar), then on the stack. Scalar results use `x0`/`w0`. The stack is
16-byte aligned. `x19`-`x28` are callee-saved.

Linux and macOS use different syscall conventions and numbers. Those details
belong to the target-specific runtime modules and do not alter the compiler
helper ABI.

## Explicit aggregate result storage

Helpers that produce aggregate source values receive an explicit writable
result pointer as their first ordinary C parameter. This is not an implicit
platform aggregate-return convention.

The canonical `repr(C)` storage types live in `rue-runtime-abi`:

- `StrBufResult` stores `{ptr, len, cap}`;
- `OptionStrBufResult` stores `{discriminant, ptr, len, cap}`; and
- `OptionIntResult` stores `{discriminant, value}`.

Compile-time size, alignment, and field-offset assertions protect these
layouts. The manifest identifies the aggregate shape for each out pointer, and
call planning materializes the concrete source-language discriminants where an
option result requires them.

Core `str` remains a non-owning packed-byte view. `std.strbuf.StrBuf` is the
source-defined growable string type. Its algorithms and destructor are ordinary
Rue code, not runtime ABI exports.

## Width-explicit float formatting

The runtime has one float-to-text authority and one production symbol,
`__rue_to_string_float`. Its scalar inputs are the raw IEEE-754 bits in a
`u64` and a separate `u32` width discriminator. The canonical discriminator
values are `FLOAT_WIDTH_F32` (32) and `FLOAT_WIDTH_F64` (64); an f32 encoding
must also have its upper 32 bits clear. Invalid encodings trap before allocation
instead of selecting a width implicitly.

The helper delegates both widths to the vendored no-std `zmij` formatter and
then copies the shortest-round-trip spelling into a fresh runtime allocation.
The ordinary `StrBufResult` out-pointer contract transfers that allocation to
the caller. Language-level formatting and debug exposure are separate compiler
and standard-library concerns.

## Safety and termination

Manifest pointer modes distinguish readable inputs, writable inputs, mutable
allocation pointers, and aggregate result storage. The runtime wrapper is
`unsafe extern "C"` whenever the caller must uphold pointer validity or layout
preconditions; helpers with no caller-side pointer obligation can use a safe
Rust boundary.

The manifest also distinguishes returning helpers from traps and process
termination. Rue does not unwind across this boundary. Runtime traps report the
diagnostic and terminate the process with the runtime-error status.

## The test failure channel

Six helpers implement the ADR-0083 §3 and §5.1 channel. Three are dispatcher
plumbing no source spelling selects; the reporting helpers are what a test
body's `?` and the comparison assertions lower to, in an ordinary executable as
well as in a test image.

- `__rue_test_normalize_process` narrows the captured argument count to one, so
  a test observes the pinned inventory rather than the selector it was
  dispatched by.
- `__rue_test_complete` writes the terminal completion record.
- `__rue_test_failure_site` and `__rue_test_fail` report one structured failure
  and abort. They are a pair because a failure record carries three byte views
  plus a file, a line, and a column — ten arguments, where every runtime helper
  is register-only and x86-64 affords six. The first stages the location, the
  second emits the record and takes the ordinary panic path; nothing runs
  between them, and the staged file bytes must stay readable across both.
- `__rue_test_fail_comparison` is the same terminal call for a comparison
  assertion (Phase 2.5, ABI version 3): it carries the two rendered operands as
  the record's `left` and `right` where `__rue_test_fail` carries a message
  and the open payload. Its message is not a parameter — it is pinned by the
  kind, `assertion failed: left == right` for `assert_eq` and
  `assertion failed: left != right` for `assert_ne` — which is what keeps a
  six-register call able to carry both operands. It pairs with
  `__rue_test_failure_site` under the same adjacency rule.
- `__rue_test_usage_error` writes one pinned diagnostic for a malformed
  selector and *returns*, unlike every other stderr-writing runtime path,
  because the dispatcher owns that case's exit status.

`@assert_eq(l, r)` and `@assert_ne(l, r)` compile to the ordinary equality
lowering plus, on the failing branch, the two rendering calls and this pair. The
lowering does not depend on whether the request is a test one: an ordinary
process simply has no descriptor 3, so the frame write fails with `EBADF` as
designed and the pinned stderr message plus exit 101 is the whole report.

The completion and failure records go to a dedicated inherited descriptor,
number 3, one JSON object per line. Writes are best-effort: a test image run by
hand has no such descriptor, so `EBADF` is expected rather than exceptional.
The channel is not a security boundary — it prevents accidental collision with
a test's own stdout, which is all its consumers are promised.

Its capability class is **hermetic-compatible, on the same grounds as stdout**:
runner-pinned, fully captured, and budgeted. ADR-0083 §5.1 records that
classification in prose until the machine-checked manifest capability field
arrives with the deferred capability ADR.

## Export classes

The reserved `__rue_` namespace contains several deliberately distinct kinds
of symbol:

- callable runtime helpers from `RuntimeHelperId`;
- target startup and signal shims from `ReservedExportId`;
- compiler-generated Rue functions and drop glue; and
- compiler-internal intrinsic presentation names.

The prefix alone is not a runtime-helper identity. Entry points and
compiler-built memory routines also have fixed non-prefixed exports and are
classified by `ReservedExportId`.

User declarations cannot use the reserved runtime/code-generation namespace or
the separately classified fixed export names. The declaration check consumes
the manifest classification rather than duplicating those names.

## Archive verification and linking

Each compiler build embeds the runtime archive for its own target. Before an
archive can be used, the compiler parses its object members and validates the
artifact against the typed manifest:

- every target-applicable helper and reserved export is defined exactly once;
- exports unavailable on that target are absent;
- no unknown reserved export is present;
- the retained read-only one-byte ABI marker has the exact current versioned
  symbol and zero value; and
- object architecture and symbol normalization match the selected target.

Synthetic mutation tests cover missing, duplicate, misspelled, stale-version,
wrong-target, and malformed marker cases. Runtime compile-time conformance and
archive inspection are complementary: one proves Rust types and intended
exports, while the other proves the bytes that will actually be linked.

The linker extracts required archive members, resolves relocations, and emits
the final ELF or Mach-O executable. Mach-O's platform-added leading underscore
is normalized at the object/archive boundary before manifest classification.

## Versioning

Compiler and runtime are lockstep components of one Rue revision. Any
incompatible helper symbol, signature, mode, aggregate shape, safety contract,
availability, or reserved-export change increments the manifest ABI version
and therefore changes the required marker symbol.

This contract is not a cross-release stable ABI. Cross-target executable
linking and embedding every foreign runtime archive are separate target-support
work; native CI for x86-64 Linux, AArch64 Linux, and AArch64 macOS collectively
validates the supported runtime archives.
