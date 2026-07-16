---
id: 0055
title: Typed compiler-runtime ABI manifest
status: accepted
tags: [architecture, compiler, runtime, abi, codegen]
feature-flag: null
created: 2026-07-16
accepted: 2026-07-16
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-355", "RUE-629", "RUE-738", "RUE-783", "RUE-826", "RUE-827", "RUE-828", "RUE-829", "RUE-830", "RUE-831", "RUE-832", "RUE-833", "RUE-834"]
---

# ADR-0055: Typed compiler-runtime ABI manifest

## Status

Accepted as the M4 typed-runtime-ABI design by RUE-826 on 2026-07-16.
This is a design-only increment. It authorizes the dependency-ordered
implementation slices below and does not itself add or migrate a runtime
helper.

## Summary

Rue will define the compiler/runtime boundary once in a dependency-light
`rue-runtime-abi` crate. An exhaustive `RuntimeHelperId` will identify every
compiler-callable runtime helper. A typed manifest entry will provide its
exported symbol, ordered parameters, explicit out-pointer or scalar return
shape, safety contract, return behavior, target availability, and calling
convention.

Compiler phases will carry helper IDs and consume manifest signatures. Raw
symbol strings will be produced only at display, object, archive, and link
boundaries. Runtime exports will prove their Rust `extern "C"` types against
the same manifest, and embedded archives will be checked against its required
exports before compiler publication.

Entry points, compiler-built memory routines, runtime-private functions,
platform signal shims, and compiler-generated Rue symbols are not callable
runtime helpers. They remain explicit, separately validated export classes.

## Context

The current compiler/runtime contract is distributed across independent
sources:

- `rue-builtins` publishes raw `runtime_fn: &str`-style constants and prose
  signatures;
- semantic analysis constructs raw external-call names and layouts for string,
  character, formatting, parsing, allocation, I/O, panic, and assertion paths;
- codegen maps CFG operations to symbol literals and repeats parts of their
  logical ABI in value, call, allocation, and target lowering;
- x86-64 and AArch64 lowerers directly intern selected helper strings;
- `rue-runtime` declares Rust `extern "C"` exports and checks pointer unsafety
  by parsing its own source text; and
- `docs/runtime-abi.md` restates signatures manually and is already stale
  relative to the source.

The archive smoke test proves that an archive parses and is nonempty. It does
not prove that every helper the compiler may call exists, that a symbol occurs
once, that it is available for the selected target, or that the Rust function
type agrees with the compiler's argument and return plan.

This duplication permits three failure classes:

1. a compiler and runtime rename disagree and fail only during linking;
2. both sides retain the same symbol but disagree on parameter order, width,
   pointer mode, or return convention and miscompile at runtime; and
3. one target archive omits or accidentally exports a helper while host-only
   tests remain green.

The `__rue_` prefix is not a sufficient identity. Compiler-generated functions,
drop glue, internal RIR intrinsics, runtime-private functions, and platform
shims also use reserved spellings. Prefix recognition therefore cannot replace
an exhaustive helper ID.

## Current ABI inventory and disposition

The following table is the accepted callable-helper inventory. RUE-828 must
create one `RuntimeHelperId` per exported helper symbol listed and preserve the
current ABI until a separate semantic change is accepted.

| Helper group | Current exported symbols | Accepted typed disposition |
| --- | --- | --- |
| Process | `__rue_exit` | `Exit`; one `i32` value parameter; never returns |
| Allocation | `__rue_alloc`, `__rue_free`, `__rue_realloc` | `Alloc`, `Free`, `Realloc`; explicit byte counts, alignment, and mutable-pointer modes |
| Arithmetic traps | `__rue_div_by_zero`, `__rue_overflow`, `__rue_intcast_overflow`, `__rue_bounds_check` | Four zero-parameter, never-returning helpers |
| Panic/assert | `__rue_panic`, `__rue_panic_no_msg`, `__rue_assert_failed` | Message pointer/length where present; never returns |
| Debug | `__rue_dbg_i64`, `__rue_dbg_u64`, `__rue_dbg_bool`, `__rue_dbg_str` | Signed, unsigned, canonical boolean-word, and text-view helpers |
| Text comparison/indexing | `__rue_str_eq`, `__rue_str_byte_at` | Packed text-view inputs; full-width scalar returns |
| Strict character iteration | `__rue_str_char_scalar`, `__rue_str_char_next` | Packed text view plus byte offset; full-width `u64` return carrying the scalar or next offset |
| Lossy character iteration | `__rue_str_char_scalar_lossy`, `__rue_str_char_next_lossy` | Same physical inputs; explicit distinct helper IDs |
| Formatting | `__rue_to_string`, `__rue_to_string_unsigned` | Explicit `StrBufResult` out pointer followed by `i64` or `u64` |
| Owned-text I/O | `__rue_print`, `__rue_println` | Borrowed three-slot `StrBuf` representation |
| Text-view I/O | `__rue_str_print`, `__rue_str_println` | Packed pointer/length view |
| Input | `__rue_read_line` | Explicit `OptionStrBufResult` out pointer plus concrete `Some`/`None` discriminants |
| Parsing | `__rue_parse_i32`, `__rue_parse_i64`, `__rue_parse_u32`, `__rue_parse_u64` | Explicit option-result out pointer, text view, and concrete discriminants |
| Randomness | `__rue_random_u32`, `__rue_random_u64` | Zero parameters with exact scalar return width |
| UTF-8 trap | `__rue_invalid_utf8` | Callable trap helper used by runtime objects today; zero parameters and never returns |

`__rue_invalid_utf8` is a normal callable helper even though its current caller
is another runtime object rather than generated program code. Giving it the
same typed identity lets archive selection, conformance, and any future
compiler mapping consume one contract.

The following names are deliberately outside `RuntimeHelperId`:

| Export class | Names or pattern | Owner and validation |
| --- | --- | --- |
| Program entry | `_start`, `_main` | Target runtime and linker entry policy |
| Platform startup/signal exports | `__rue_x86_64_linux_start`, `__rue_rt_sigreturn` | Target runtime implementation; target-specific archive export allowlist |
| Internal runtime callbacks | `__rue_stack_overflow_handler` | Rust function-type conformance only; this is not an unmangled archive ABI symbol |
| Compiler-built memory | `memcpy`, `memmove`, `memset`, `memcmp`, `bcmp` | Rust compiler lowering contract; typed separately from Rue helpers |
| Runtime-private functions | UTF-8 decoder helpers and ordinary private Rust functions | Not exported; ordinary Rust checking |
| Compiler-generated Rue symbols | `__rue_fn_*`, `__rue_drop_*`, `__rue_drop_array_*` | Compiler symbol mangling and drop-glue ownership |
| Compiler-internal intrinsics | `__rue_iter_len`, `__rue_char_scalar`, `__rue_char_next`, and lossy variants | Typed RIR identity owned by RUE-827, never an exported ABI symbol |

New intentionally externally visible global names must enter one of these
classes. An unclassified externally visible export is an error rather than an
implicit helper; ordinary mangled Rust implementation symbols are not part of
this inventory.

## Decision

### A dependency-light Rust manifest crate

RUE-828 will add a leaf crate named `rue-runtime-abi`. It must support `no_std`,
perform no allocation or initialization, and depend on no compiler IR, semantic
type pool, codegen, linker, or runtime implementation crate. `rue-builtins`,
`rue-air`, `rue-cfg`, `rue-codegen`, `rue-compiler`, and `rue-runtime` may
depend on it.

The canonical data is ordinary Rust declarations and `const` tables. A build
script, external schema parser, or generated checked-in source would introduce
a second tool and failure path without improving this fixed, small ABI. Macros
may remove repetition inside the crate, but their expansion is not a second
manifest.

The public conceptual model is:

```text
RuntimeHelperId
RuntimeHelper {
    id
    symbol
    parameters: &[AbiParameter]
    result: AbiResult
    safety: SafetyContract
    return_behavior: Returns | Never
    availability: TargetSet
    calling_convention: TargetC
}

AbiParameter {
    ty: AbiType
    mode: Value | ConstPointer | MutPointer | OutPointer(AggregateShape)
}

AbiResult = Void | Scalar(AbiType)
```

Names and exact Rust spelling may differ, but the information and ownership may
not. Explicit out pointers are parameters, not inferred platform sret
registers. `StrBufResult`, `OptionStrBufResult`, and integer option results are
named aggregate shapes whose slot types and order are declared once.

`AbiType` describes the physical C-boundary scalar, not a Rue semantic type:
the initial set includes exact signed and unsigned integer widths, the
canonical boolean word used by the current ABI, and opaque byte/result
pointers. Aggregate layout, source type identity, and target register
assignment remain outside the manifest.

### Stable identity, lookup, and string boundaries

`RuntimeHelperId` is exhaustive and cannot be constructed from an arbitrary
string. The manifest provides:

- `RuntimeHelperId::ALL` or an equivalent complete iterator;
- total ID-to-entry lookup;
- checked symbol-to-ID lookup for archive and linker boundaries;
- `Display` or `symbol()` for emitted diagnostics and relocations; and
- validation that IDs and exported symbols are unique.

Compiler IR and planning structures carry `RuntimeHelperId`. Conversion to a
symbol occurs only when formatting debug output, interning a target MIR
relocation, writing an object, inspecting an archive, or linking. User external
functions and compiler-generated Rue functions retain their existing distinct
symbol identities.

Source builtin and intrinsic owners map their typed operations to
`RuntimeHelperId`. The leaf crate does not depend on their enums and does not
store source-language strings. Exhaustive mapping tests in those owning crates
prove that each applicable builtin or intrinsic selects a real helper and uses
the canonical signature.

### Signature and call-plan ownership

The manifest owns logical C-boundary facts:

- ordered scalar and pointer parameters;
- explicit out-pointer aggregate shape;
- scalar or void result;
- never-returning behavior;
- pointer mutability and required validity;
- helper availability; and
- the target C calling-convention class.

The manifest does not own:

- Rue source types or type inference;
- CFG values, slots, places, or aggregate materialization;
- x86-64 or AArch64 registers and stack offsets;
- target instruction selection, flags, encodings, relocations, or clobber
  register sets; or
- object/archive parsing.

Existing shared call planning remains the canonical physical assignment path.
It consumes a manifest signature plus materialized operands. Target adapters
derive registers, stack locations, and caller-saved clobbers from
`TargetC`; per-helper register lists are forbidden unless a future helper uses
a genuinely different convention.

Widths and extension are explicit at the boundary. In particular,
`__rue_dbg_bool` currently consumes an `i64` word, `__rue_str_eq` returns a
full-width `u64`, both character-scalar helpers return `u64` even though sema
exposes a `u32` source value, and `__rue_random_u32` returns `u32`. Consumers
may not infer these facts from a Rue semantic type or a symbol name.

### Safety and checked requirements

Each helper declares a `SafetyContract` sufficient to distinguish:

- no pointer preconditions;
- readable pointer/length input;
- writable allocation or result storage;
- allocation layout requirements;
- concrete enum discriminants supplied by the caller; and
- unconditional trap/termination.

This metadata is a compiler/runtime invariant and diagnostic aid, not a new
source-language effect system. RUE-830 validates compiler-constructed operands
against the manifest's types and modes. RUE-832 checks whether the Rust export
is `unsafe extern "C"` whenever raw-pointer validity is a caller obligation.

### Target availability and shims

The initial callable helper set is available on all three runtime targets:
x86-64 Linux, AArch64 Linux, and AArch64 macOS. The manifest represents
availability explicitly so a later target-specific helper cannot silently
appear universal.

Entry points, signal trampolines, startup functions, and compiler-built memory
routines use separate typed/allowlisted export records. They are checked by
RUE-832 and RUE-833 but cannot be selected as `RuntimeHelperId`.

### Runtime conformance

RUE-832 will make each Rust export prove its function type against the manifest
at compile time. The implementation may use manifest-generated function-pointer
assertions or macros that declare both the manifest row and the typed assertion.
Source-text parsing is not sufficient because it cannot reliably prove
parameter order, result type, cfg applicability, or a single implementation.

For each target, conformance must prove:

1. every applicable helper has exactly one implementation;
2. the declaration macro or wrapper owns the expected unmangled export
   attribute and symbol mapping;
3. the Rust C function type matches the ordered manifest signature;
4. pointer-bearing exports have the required unsafe contract; and
5. separately classified shims satisfy their own typed record.

RUE-833 independently inspects every embedded archive. Compile-time type
assertions/declaration ownership and archive symbol validation are
complementary: the former proves Rust types and the intended export spelling,
while the latter proves the artifact actually contains that spelling.

### Versioning

Compiler and runtime remain lockstep components of one Rue release. The
manifest exposes a monotonically increasing integer ABI version. Any
incompatible symbol, signature, mode, aggregate shape, safety, or availability
change increments it.

Each runtime archive exports one metadata object symbol named
`__rue_runtime_abi_vN`, where `N` is the decimal manifest version. It is an
unmangled, retained, one-byte read-only static whose value is zero; the version
is encoded only in the symbol name, so archive validation need not interpret
target object data. It is not callable and is not a `RuntimeHelperId`.

Object readers compare normalized linker names: Mach-O's platform-added leading
underscore is removed in the same way as other parsed symbols before matching.
RUE-833 requires exactly the expected marker, rejects any other
`__rue_runtime_abi_v*` marker, and rejects multiple definitions before compiler
publication. During M4, changing the manifest and runtime implementation in the
same repository is allowed; compatibility across released compiler/runtime
pairs is not promised.

### Documentation

`docs/runtime-abi.md` becomes a consumer of the canonical classification. Where
practical, tables are generated or verified from manifest data. Handwritten
sections may explain target calling conventions and invariants but may not
restate an unverified peer signature table. RUE-834 removes current stale
signature declarations.

## Implementation slices and dependencies

| Issue | Scope | Dependency |
| --- | --- | --- |
| RUE-827 — Replace compiler-internal intrinsic strings with a typed enum | Add exhaustive RIR identity; make desugaring produce it and inference/sema consume it; prevent source-name collision. This is compiler identity cleanup, not a runtime-helper implementation. | RUE-826 |
| RUE-828 — Add canonical runtime helper IDs and typed signatures | Add `rue-runtime-abi`, the complete helper and ABI-version metadata, validation, display, and representative non-migrating consumers. | RUE-826 |
| RUE-829 — Migrate builtin runtime mappings | Replace builtin raw helper names and signature restatements with typed mappings. | RUE-828 |
| RUE-830 — Migrate directly lowered semantic calls | Build AIR calls from manifest records, validate operands, and preserve typed helper identity through AIR-to-CFG lowering. | RUE-828 |
| RUE-831 — Migrate runtime helper emission | Consume typed CFG helper calls in shared call planning and both target backends; convert to symbols only at MIR relocation/display boundaries. | RUE-828 |
| RUE-832 — Add runtime-side conformance | Prove Rust export types and exactly one applicable implementation on every target, own export attributes, and emit the target archive's ABI-version marker. | RUE-828 |
| RUE-833 — Validate embedded runtime exports | Inspect every target archive for required, duplicate, stale, and wrong-target exports and ABI version. | RUE-829, RUE-830, RUE-831, RUE-832 |
| RUE-834 — Remove remaining raw literals | Restrict string conversion to boundaries, add an inventory guard, update documentation, and run the full ABI/cross-target matrix. | RUE-827, RUE-833 |

RUE-827 and RUE-828 may proceed in parallel after this decision. RUE-829
through RUE-832 may proceed in parallel after RUE-828. RUE-833 is the
convergence check, and RUE-834 is the final cleanup.

No slice may redesign Rue's source semantics, native ABI, aggregate layout, or
target matrix as a convenience. A discovered ABI disagreement is a bug to fix
or a separately accepted change, not an opportunity to make the manifest match
one accidental consumer.

## Validation obligations

The M4 implementation must include:

- manifest uniqueness, exhaustive lookup, and aggregate-shape tests;
- focused builtin, intrinsic, semantic-call, call-plan, and cross-backend tests;
- compile-time runtime signature checks for all three targets;
- mutated archive tests for missing, duplicate, stale-version, misspelled, and
  wrong-target exports, parameterized by the expected target;
- native x86-64 and AArch64 runtime/CLI execution;
- cross-target lowering, assembly, and object validation; native archive
  validation in each of the three target CI jobs; and
- a final production-source inventory that forbids raw helper literals outside
  canonical tables, runtime definitions, display/link boundaries, and tests.

The generated-oracle isolation rule and the full-suite serialization rule in
`AGENTS.md` apply. Passing host execution alone is not sufficient evidence.
M4 does not embed all target runtime archives into one compiler or enable
foreign-target executable linking. Each compiler build continues to embed and
validate its target-matching runtime archive; the three native CI jobs
collectively cover all supported archives. Per-target embedding and
cross-target executable linking remain owned by RUE-600 and M9.

## Consequences

### Positive

- Runtime helper renames and signature changes become compile-time or
  publication-time failures instead of link/runtime surprises.
- Builtins, sema, CFG/call planning, both backends, runtime exports, archive
  validation, and documentation consume one contract.
- An exhaustive helper ID prevents raw strings and prefix heuristics from
  creating accidental ABI identities.
- Target-specific instruction and calling-convention facts remain visible in
  their correct owners.

### Negative

- The new leaf crate and conformance machinery add deliberate structure around
  a currently informal boundary.
- Every helper addition or incompatible change must update the manifest,
  implementation, validation, and ABI version together.
- Migrating existing raw-string paths touches several compiler phases and both
  backends, requiring staged PRs and cross-target CI.

### Neutral

- This decision does not stabilize the runtime ABI across Rue releases.
- It does not change source-language semantics or introduce a preview feature.
- It does not genericize target MIR, registers, encoders, or object formats.

## Rejected alternatives

### Keep `docs/runtime-abi.md` as the contract

Prose is useful explanation but cannot enforce Rust function types, archive
contents, exhaustive compiler mappings, or target availability. The current
document has already drifted from implementation.

### Generate the manifest by parsing runtime Rust source

Source parsing duplicates Rust's type system, is cfg-fragile, and cannot prove
that compiler call construction consumes the same data. Runtime declarations
must instead type-check against explicit shared Rust data.

### Use exported symbols as IDs

Strings permit fabrication, collide with other reserved symbol classes, and do
not force exhaustive matches. Symbols are a boundary representation of a typed
ID, not the compiler's identity.

### Put the manifest in `rue-builtins`, `rue-air`, or `rue-runtime`

Each choice creates an inverse dependency or blurs source semantics with the
binary contract. A leaf crate keeps the runtime freestanding and lets all
consumers share the same declarations.

## Future work

- Stabilizing a runtime ABI across compiler releases.
- Foreign runtime implementations or dynamic runtime negotiation.
- Target C ABI and general foreign-function signature classification.
- Canonical physical type layout under ADR-0052.
