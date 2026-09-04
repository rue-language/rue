---
id: 0065
title: "Floating point: f32/f64, IEEE-754 semantics, and register classes"
status: implemented
tags: [types, semantics, codegen, numerics, abi]
feature-flag: (none - feature stabilized)
created: 2026-07-19
accepted: 2026-07-20
implemented: 2026-09-04
spec-sections: ["2.1:29", "2.1:30", "3.7:22", "3.12:1", "3.12:2", "3.12:3", "3.12:5", "3.12:6", "3.12:7", "3.12:8", "3.12:9", "3.12:10", "3.12:11", "3.12:13", "3.12:14", "3.12:16", "3.12:17", "3.12:18", "3.12:19", "3.12:21", "3.12:22", "3.12:23", "3.12:24", "3.12:25", "3.12:27", "3.12:28", "3.12:29", "3.12:31", "3.12:32", "3.12:34", "3.12:35", "3.12:36", "3.12:37", "3.12:39", "3.12:40", "3.12:41", "3.12:42", "3.12:44", "3.12:46", "3.12:47", "3.12:48", "3.12:49", "4.1:13", "4.1:14", "4.2:14", "4.3:2", "4.3:5", "4.13:139", "4.13:140", "4.13:141", "4.13:142", "4.13:143", "8.1:7"]
superseded-by:
---

# ADR-0065: Floating point — f32/f64, IEEE-754 semantics, and register classes

## Status

Accepted 2026-07-20 by Steve Klabnik, ratifying the proposal below and the four
open questions resolved in the amendment (Decision §§1, 4, 8, 9). This was the
design gate for **M9 · Floating point** (RUE-714); the M9 implementation epic
and per-phase sub-issues are filed next, followed by the `PreviewFeature::Floats`
gate. Before this ADR, Rue had **zero** float support: the lexer emitted only
`Int(u64)` (`crates/rue-lexer/src/logos_lexer.rs`), the packed `Type` encoding
had no F32/F64 tags (`crates/rue-air/src/types.rs`), neither backend had XMM/V
registers, and the register allocator had **no register-class concept at all**
(`crates/rue-codegen/src/vreg.rs` is `struct VReg(u32)`, single-class). The only
existing traces were a forward reference in spec `4.3` ("once floating-point types
exist") and `comptime_float` marked *future* in [ADR-0025](0025-comptime.md).

Implemented 2026-09-04. All ten phases have landed; the normative specification
is spec chapter 3.12 plus the paragraphs listed in `spec-sections`, and the
decisions the implementation forced are recorded in Amendment 1 below. The 4.3
forward reference and the ADR-0025 *future* marker were both replaced by the
shipped rules.

## Summary

Rue gains two floating-point types, **`f32`** (IEEE-754 binary32) and **`f64`**
(binary64), with `f64` as the default. Arithmetic follows **IEEE-754 exactly**:
`/0.0` yields `±inf`, `0.0/0.0` yields `NaN`, and nothing traps — the deliberate
opposite of integer division, which traps on `/0`. There are **no implicit
conversions** — not int↔float, not `f32`↔`f64`; every width or domain change is
an explicit intrinsic (`@int_to_float`, `@float_to_int`, `@float_cast`). Literals
are **`comptime_float`** (arbitrary precision, per ADR-0025), coerced to a
concrete type by context with round-to-nearest, so no literal suffixes are
introduced. Comparison operators (`==`, `<`, …) use **IEEE partial** semantics —
`NaN != NaN`, `NaN` is unordered — which formally resolves the open question in
spec 4.3 and is the concrete motivation for the future `PartialEq`/`Eq` split
(RUE-246). A **`@total_cmp` intrinsic ships with v1** as a stopgap total order
(IEEE `totalOrder`) for sorting and hashing, ahead of that trait split. The
enabling compiler change is a **register-class split (GP vs FP)**
threaded through `VReg`, liveness, register allocation, and scheduling; it lands
as a no-op refactor *before* any float instruction selection. Runtime `float →
string` adopts the already-vendored `zmij` dtoa crate. The feature ships behind
`PreviewFeature::Floats` until Phase 10 retires that gate.

## Context

Floats do not shard into small independent issues: the literal grammar, the type
tags, the inference rule, both backends, the calling convention, and the runtime
formatter are one decision wearing many hats. Two things in particular make this
a genuine design gate rather than a mechanical port:

1. **The register-class change is structural.** Every prior type in Rue lives in
   a general-purpose register. Floats live in a *separate* register file (SSE/XMM
   on x86-64, the FP/NEON V-registers on aarch64) that the allocator, liveness,
   and scheduler must model as a distinct class. `VReg` is currently classless.
   This is the deepest lift and it touches both backends at once.

2. **IEEE-754 collides with two of Rue's own principles.** `NaN != NaN` breaks
   the "structural equality is a total equivalence" invariant that
   [ADR-0035](0035-string-model-byte-strings.md) and spec 4.3 rely on. And float
   arithmetic is *fully defined for every input* (`÷0 → ±inf`, invalid `→ NaN`,
   overflow `→ inf`), which is **more** defined than integer division — so
   [ADR-0036](0036-behavior-classification-preference.md) ("prefer the
   most-defined category") actually points *toward* the non-trapping IEEE
   behavior, even though it diverges from how integers behave.

### Prior art

The six languages the design was checked against cluster cleanly on the choices
that matter.

| Language | Default literal type | Implicit int↔float | Implicit f32↔f64 | `NaN == NaN` | Literal model |
|---|---|---|---|---|---|
| **C** | `double` | yes (usual arithmetic conversions) | yes (widening) | false | typed constants, `f`/`l` suffixes |
| **Go** | `float64` (untyped const) | no (untyped constant coercion only) | no | false | arbitrary-precision untyped constants |
| **Rust** | inferred, else `f64` | no (`as` casts only) | no (`as` casts) | false (`PartialOrd`; `total_cmp` for total) | typed, `f32`/`f64` suffixes |
| **Zig** | `comptime_float` (→ `f64` default) | no (`@floatFromInt`/`@intFromFloat`) | widening coerces; narrowing explicit | false | `comptime_float` = f128 precision, no suffixes |
| **Swift** | `Double` | no (explicit `Double(x)`) | no (explicit) | false (`==` is IEEE; separate total order) | `ExpressibleByFloatLiteral` |
| **Hylo** | `Float64` | no | no | false | stdlib literal conformance |

The consensus is strong and modern: **f64 default, no implicit conversions, IEEE
comparison**. The two real disagreements are (a) *how* literals reach a concrete
type — C/Rust use typed constants and suffixes, Go/Zig use an abstract
arbitrary-precision constant that coerces by context — and (b) how to reconcile
IEEE partial ordering with containers/sorting/hashing that need a total order.

## Decision

Take the modern consensus wholesale and resolve the two disagreements in the
direction of Rue's existing grain.

### 1. Types and IEEE-754 semantics

- **Two types: `f32` (binary32) and `f64` (binary64).** Both, not f64-only:
  `f32` earns its place for memory footprint, GPU/graphics interop, and C FFI
  (RUE-1059), and once register classes exist a second float width is nearly
  free. `f64` is the **default** wherever inference has no other signal
  (Go/Swift/Hylo/C consensus).
- **Arithmetic is IEEE-754, and total.** `+ - * / %` map to hardware scalar FP
  ops. Division by zero does **not** trap: `x/0.0 → ±inf`, `0.0/0.0 → NaN`,
  overflow `→ ±inf`, invalid `→ NaN`. This is the deliberate divergence from
  integer division (which traps on `/0`, spec 4.2), and the spec must state it
  explicitly. It is *consistent* with [ADR-0036](0036-behavior-classification-preference.md):
  IEEE floats are the most-defined category — no operation is undefined, so none
  needs a trap.
- **`-0.0`, `inf`, `NaN` are ordinary runtime values.** There is no `NaN`/`inf`
  literal syntax (matching Zig); they arise from arithmetic or from `std.math`
  constants/intrinsics (`@is_nan`, `std.math.inf`, `std.math.nan`).
- **`f16`/`f128` are out of scope**, confirmed on ratification: no hardware
  `f16` on baseline targets, and `f128` needs soft-float. Their only foreseeable
  interaction with Rue is FFI, where `_Float16` and `long double` are simply
  non-FFI-safe types under the existing RUE-740/742 FFI-safety predicates —
  which already express that without new work. Reconsider only with a concrete
  need.

### 2. Comparison and equality (resolves spec 4.3)

- **Operators are IEEE partial.** `==`, `!=`, `<`, `<=`, `>`, `>=` on floats
  follow IEEE-754: `NaN` compares **unordered** (`NaN == NaN` is false, every
  ordering against `NaN` is false), and `-0.0 == +0.0` is true. This is what all
  six surveyed languages do and what the hardware compare instructions
  (`ucomisd`, `fcmp`) produce; deviating would be both surprising and slow.
- **This is the motivating case for the `PartialEq`/`Eq` split.** Spec 4.3
  already predicts it: today structural equality is a total equivalence because
  no leaf type has a value that is unequal to itself. `f32`/`f64` introduce
  exactly such a value. Under this ADR, a structural `==` on an aggregate
  containing a float leaf inherits IEEE partiality (a struct holding a `NaN` is
  not equal to itself). We accept that as the honest consequence. A usable total
  order ships immediately as the `@total_cmp` intrinsic (§8) rather than waiting
  on traits — but the **trait machinery itself** (`Eq`/`Ord`, RUE-246) is still
  deferred: `@total_cmp` is a stopgap primitive today and becomes the literal
  implementation of `Ord::cmp` once that split lands. No trait work is in scope
  here; this ADR only records that float `==` is IEEE, that the split is now
  *required* rather than merely anticipated, and that `@total_cmp` closes the
  gap in the meantime.

### 3. Literals and inference

- **Literals are `comptime_float`** — arbitrary precision, per the ADR-0025
  table, mirroring `comptime_int`. Grammar: `1.5`, `1e9`, `1.5e-3`, `6.022e23`
  (a digit run, then either a `.` fraction or an exponent or both). A leading or
  trailing dot (`.5`, `5.`) is **rejected** for the same readability reason Rue
  rejects other ambiguous lexemes; write `0.5` / `5.0`.
- **No literal suffixes.** Rue has no integer suffixes either (the lexer's only
  numeric token is `Int(u64)`); adding float suffixes would be a new precedent
  for no gain. A `comptime_float` coerces to the concrete type demanded by
  context — `let x: f32 = 1.5;`, `fn f(y: f64)`, `1.0 + x` — exactly as
  `comptime_int` already does. This is the Go/Zig abstract-constant model, chosen
  over C/Rust typed-suffix constants because it composes with Rue's existing
  comptime story.
- **Coercion rounds to nearest.** A `comptime_float` that is not exactly
  representable in the target type is rounded round-half-to-even (IEEE default).
  A literal whose magnitude exceeds the target's finite range is a **compile
  error**, not a silent `inf` (matches Rust's "literal out of range").
- **`comptime_int` → `comptime_float`** is allowed (an integer literal is valid
  where a float is expected: `let x: f64 = 3;`), mirroring Zig. The reverse
  (`comptime_float` → integer) is **not**, even for integral values.

### 4. Conversions: explicit only

No implicit int↔float or f32↔f64 conversion ever happens at runtime. Three
intrinsics, named in Rue's existing `@x_to_y` convention (`@int_to_ptr`,
`@ptr_to_int`):

- **`@int_to_float(x)`** — integer → float, rounding to nearest.
- **`@float_to_int(x)`** — float → integer, **truncating toward zero**, and it
  **traps** on `NaN` or an out-of-range magnitude. Confirmed on ratification.
  Trapping (rather than Rust's saturating `as` or C's UB) is the loud-pragmatism
  choice and is consistent with Rue's trapping integer overflow. Recorded
  honestly: hardware does not trap for us — `cvttsd2si`/`fcvtzs` produce
  sentinel values on `NaN`/out-of-range input rather than faulting — so every
  `@float_to_int` lowers to a compare-and-branch plus the convert instruction,
  not the convert alone. That per-call check is exactly why Rust chose
  saturating `as` instead; the "saturating variant later if a concrete need
  appears" escape hatch above exists to absorb that cost if it proves to
  matter.
- **`@float_cast(x)`** — `f32` ↔ `f64`. Widening is exact; narrowing rounds to
  nearest (and may produce `±inf`). Both directions are explicit precisely
  because narrowing is lossy — the same "lossiness is never implicit" discipline
  as [ADR-0035](0035-string-model-byte-strings.md).

### 5. Codegen: register classes first, then float instructions

This is the structural pre-work and it must land **before** any float
instruction selection, as its own reviewed change.

- **Add a `RegClass` (GP, FP) to `VReg`** and thread it through liveness,
  register allocation, and scheduling. The refactor is a **no-op**: every
  existing virtual register is `RegClass::Gp`, so behavior is identical and the
  change is validated by the existing suite passing unchanged. Physical register
  sets, spill slots, and move insertion become class-aware.
- **x86-64:** SSE2 scalar ops (`movss`/`movsd`, `addsd`, `subsd`, `mulsd`,
  `divsd`, `sqrtsd`, `cvtsi2sd`/`cvttsd2si`, `cvtss2sd`/`cvtsd2ss`), the XMM0–15
  register file, and `ucomisd`/`ucomiss` for comparisons (unordered → the IEEE
  partial result). SSE2 is baseline for x86-64, so no feature detection.
- **aarch64:** scalar FP/NEON ops (`fadd`, `fsub`, `fmul`, `fdiv`, `fsqrt`,
  `scvtf`/`fcvtzs`, `fcvt`), the V0–31 register file (Sn/Dn views), and `fcmp`.
- **Calling conventions:** SysV passes/returns floats in XMM0–7 / XMM0; AAPCS64
  in V0–7 / V0. Both backends land in the **same PR** per the repository's
  multi-backend rule, with encoding and execution tests each. Where the two
  backends share lowering shape, prefer the shared middle layer of
  [ADR-0048](0048-shared-codegen-middle-layer.md) rather than duplicating.

### 6. Runtime and formatting

- **`float → string` adopts the vendored `zmij` dtoa crate** (`third-party/vendor/zmij-0.1.7`,
  a shortest-round-trip decimal converter), wired as `__rue_to_string_float`.
  A host-only runtime symbol is fine here because `@to_string` already routes
  through the runtime. This unblocks printing, `@dbg`, and `std.fmt` for floats.
- **`string → float` (`strtod`) is deferred.** Nothing in v1 needs runtime float
  parsing — literals are handled at compile time by the lexer/comptime path, not
  by a runtime parser. A later ADR/issue can add it alongside the broader parsing
  surface.

### 7. Standard library surface

- **`std.math`:** the float operations that are a *single hardware instruction* —
  `sqrt`, `floor`, `ceil`, `trunc`, `round` (via `roundsd` / `frintX`) — plus the
  `inf`/`nan`/`is_nan` primitives. Transcendentals (`sin`, `cos`, `exp`, `log`,
  `pow`) are **deferred**: they need a `libm`-class implementation and Rue has no
  libm dependency. `std.math` today is integer-only; the float functions are
  additive.
- **`std.fmt`:** float formatting through the dtoa runtime (default shortest
  round-trip; precision/width control can follow).
- **`@dbg`** gains float support.

### 8. Total ordering: `@total_cmp` ships with v1

Resolves the "total-order timing" open question. An intrinsic, not a stopgap
trait, lands with v1 rather than waiting for the `PartialEq`/`Eq`/`Ord` split
(RUE-246).

- **Rationale.** With IEEE-partial `==` as the only comparison (Decision §2),
  sorting a float slice containing `NaN` is ill-defined, and hash containers
  keyed on floats violate the hash/equality contract: `NaN != NaN` makes a
  `NaN` key unfindable, and `-0.0 == +0.0` across distinct bit patterns breaks
  naive bit-hashing. Without an escape hatch, total ordering may be
  inexpressible in pure Rue at all — the hand-rolled workaround needs a
  float→int bitcast, and today's conversion intrinsics (§4) are all value
  conversions, never reinterpretations.
- **Semantics:** IEEE `totalOrder` — `-0.0 < +0.0`, NaNs ordered by sign and
  payload bit pattern, exactly as specified by IEEE 754-2008 §5.10.
- **Return type:** a signed integer, memcmp-style (negative/zero/positive; no
  `Ordering` enum until traits exist).
- **Precedent:** this is the same intrinsic-now-trait-later pattern already
  used for the FFI-safety predicates (RUE-740 → RUE-504). Commitment risk is
  low: IEEE `totalOrder` is standardized and both Rust (`f64::total_cmp`) and
  Swift converged on it, so `@total_cmp` becomes the literal implementation of
  `Ord::cmp` once RUE-246 lands — nothing here needs to be redesigned later.
- **Not resolved here:** whether to add a general `@bit_cast` intrinsic (useful
  beyond floats, and the thing whose absence made deferring `@total_cmp` risky
  in the first place). Tracked as a follow-up in Future Work rather than
  decided in this ADR.

### 9. `%` on floats: deferred, not in v1

Resolves the "`%` on floats" open question. The `%` operator stays
integer-only; the typechecker rejects it on float operands with a diagnostic
that points at the `std.math.rem` workaround path
(agents porting C/Rust code with `fmod` will hit this rejection directly).

Deferral is deliberate, not just scope-trimming: truncated `fmod` and IEEE
round-to-even remainder differ and both have prior-art claims (C/Rust use
`fmod`; IEEE 754 defines `remainder` as round-to-even), and there is no
concrete use case yet to pick a winner. `std.math.rem` can land as an explicit
function alongside transcendentals, or sooner if a concrete need appears
first.

## Implementation Phases

Sub-issues are filed under an M9 epic **after this ADR is accepted**; the
register-class pre-work (Phase 1) is a hard prerequisite for Phases 5–6.

- [x] **Phase 1: Register classes (GP/FP) in codegen** — no-op refactor of
  `VReg`/liveness/regalloc/scheduling. RUE-NNN
- [x] **Phase 2: Lexer** — `comptime_float` literal token (`1.5`, `1e9`). RUE-1068
- [x] **Phase 3: Parser + RIR** — float literal node through untyped IR. RUE-1069
- [x] **Phase 4: AIR types + inference** — `f32`/`f64` tags in the packed `Type`,
  `comptime_float`, context coercion, `@int_to_float`/`@float_to_int`/`@float_cast`;
  plus `@total_cmp` typing (§8) and rejecting `%` on float operands with a
  diagnostic naming the `std.math.rem` path (§9). RUE-1070
- [x] **Phase 5: x86-64 backend** — SSE2 scalar ops, XMM regs, SysV FP ABI. RUE-NNN
- [x] **Phase 6: aarch64 backend** — FP/NEON scalar ops, V-regs, AAPCS64 FP ABI. RUE-NNN
- [x] **Phase 7: Runtime dtoa** — wire `zmij` as `__rue_to_string_float`. RUE-1073
- [x] **Phase 8: std.math / std.fmt / @dbg** — hardware-instruction math, float
  formatting, `@total_cmp` lowering (§8). RUE-NNN
- [x] **Phase 9: Spec + spec tests** — a `03-types/` float chapter, division-divergence
  and NaN-comparison paragraphs, plus paragraphs for total ordering (`@total_cmp`,
  §8) and the deferred float `%` (§9), traceability. RUE-NNN
- [x] **Phase 10: Stabilization** — remove the `Floats` preview gate. RUE-1076


## Amendment 1: decisions taken during implementation (2026-09-04)

Ratification left these open at the level of "the obvious thing"; implementing
Phases 1–9 forced each one to a concrete answer. They are recorded here rather
than in a new ADR because none of them changes a Decision above.

1. **x86-64 rounding uses SSE4.1, the one instruction above the SSE2
   baseline.** Decision §5 promised SSE2-only lowering, which has no rounding
   instruction at all: `@floor`/`@ceil`/`@trunc`/`@round` under SSE2 alone cost
   a magic-constant add-subtract sequence with its own edge cases. The backend
   instead emits `roundsd`/`roundss` (SSE4.1) for `@floor`, `@ceil`, and
   `@trunc`, and a trunc-and-adjust sequence built on `roundsd`/`roundss` for
   the ties-away-from-zero `@round`, which no single instruction provides on
   either target. This raises the x86-64 floor from SSE2 to SSE4.1 for these
   five intrinsics only; every other float operation stays on the SSE2
   instructions §5 names. SSE4.1 has been present on shipping x86-64 parts
   since 2008 and is well below the 64-bit baseline any Rue target meets.
   AArch64 needs no equivalent: `frintX` covers all five directly.

2. **Comptime float arithmetic evaluates at the operation's width, not in
   arbitrary precision.** Decision §3 calls `comptime_float` arbitrary
   precision, which is true of a *literal*: its written text is carried exactly
   until context fixes a width. It is not true of an *operation*. A `const`
   expression's `+`, `-`, `*`, `/`, negation, and comparisons are evaluated at
   the width of the operation — the annotated type, or the `f64` default when
   only literals are involved — so a `const` and the identical runtime
   computation cannot disagree. Evaluating at higher precision and rounding
   once at the end would make `const SUM: f64 = 0.1 + 0.2;` differ from the
   runtime sum, which is the double-rounding trap C's `long double` intermediate
   is famous for. This is spec 3.12:46 and 3.12:47. Its corollary is that a
   comptime expression mixing an `f32` operand with an `f64` one has no width
   to evaluate at and is rejected (3.12:49), matching the runtime rule.

3. **A comptime NaN is canonicalized to a positive quiet NaN.** Compile-time
   evaluation runs on the build host, so an uncanonicalized NaN would carry the
   *host's* default sign into the compiled program and make the output
   host-dependent — a reproducible-build break. The comptime evaluator therefore
   renders every NaN as a positive quiet NaN (spec 3.12:48).

4. **The sign of a runtime NaN is target-defined, and specified as
   implementation-defined.** The default NaN the hardware produces for an
   invalid operation is negative on x86-64 and positive on AArch64, and forcing
   agreement would cost a fixup on every float operation that can produce one.
   The sign is observable only through `@total_cmp` or bit inspection, never
   through arithmetic, the comparison operators, or formatting. Spec 3.12:44
   classifies it as implementation-defined (Appendix B.1) — a documented choice
   from a permitted set — explicitly *not* as undefined behavior.

5. **Floats cross the runtime helper ABI as bit patterns in `u64` beside an
   explicit width.** The compiler-to-runtime boundary (`rue-runtime-abi`) stays
   integer-only: a float argument to a runtime helper is passed as its IEEE bit
   pattern in a `u64` general-purpose parameter and reinterpreted inside the
   helper. The width travels with it as a separate `u32` discriminator —
   `FLOAT_WIDTH_F32` (32) or `FLOAT_WIDTH_F64` (64) — rather than being encoded
   in the symbol, so each float-consuming helper is a single width-explicit
   authority: `__rue_to_string_float(out, bits, width)` and
   `__rue_dbg_float(bits, width)`, the latter formatting through the former. An
   f32 pattern is zero-extended and must leave the upper 32 bits clear; any
   other encoding traps rather than selecting a width implicitly. This keeps the
   ABI contract free of any floating-point calling-convention dependency, so the
   runtime needs no FP register classification of its own; the Rue-level
   *language* calling conventions (SysV XMM0–7, AAPCS64 V0–7) are unaffected and
   remain as Decision §5 specifies.

6. **Formatting is zmij shortest-round-trip with fixed notation thresholds.**
   Decision §6 adopted `zmij` without saying what the rendered text looks like.
   It is the shortest decimal digit string that round-trips at the value's own
   width, laid out positionally when the decimal exponent is in `-5..=15` for
   `f64` or `-6..=12` for `f32`, and in `d.ddde±XX` scientific form with an
   explicit exponent sign otherwise. The rendering always carries a fractional
   part or an exponent — `1.0` prints as `1.0`, never `1` — so float output is
   never mistakable for integer output. Specials print `NaN`, `inf`, `-inf`,
   and negative zero prints `-0.0`. This is spec 3.12:40–3.12:42.

7. **`std.math.rem` shipped with the float functions rather than waiting.**
   Decision §9 deferred float `%` and named `std.math.rem` as the workaround
   path "once a concrete use case picks a semantics". The rejection diagnostic
   points at that function, so it had to exist for the diagnostic to be
   actionable. It implements the truncated remainder (C's `fmod`), computed
   exactly by shift-and-subtract, which is the semantics the `%` spelling would
   have had in C and Rust. The operator itself remains undefined on floats
   (spec 3.12:25); this fixes only the library function's semantics.

## Consequences

### Positive
- Rue gets real, hardware-speed IEEE-754 arithmetic on both backends, matching
  the strong cross-language consensus (no implicit conversions, f64 default,
  IEEE comparison) — nothing here will surprise a Rust, Go, Swift, or Zig user.
- The register-class split is a **reusable capability**, not float-specific
  plumbing: SIMD, fixed-register intrinsics, and future multi-class work all
  build on it.
- The `comptime_float` literal model composes with the existing comptime story,
  so no new suffix grammar and no new constant-typing rules are introduced.
- Non-trapping IEEE arithmetic aligns with ADR-0036, while trapping
  `@float_to_int` aligns with Rue's loud-pragmatism trapping elsewhere — each
  choice is justified by an existing principle, not invented ad hoc.

### Negative
- **IEEE `==` breaks total structural equality.** An aggregate containing a
  float leaf is no longer guaranteed equal to itself (a struct holding `NaN`).
  This forces the `PartialEq`/`Eq` split (RUE-246) to become required work rather
  than a someday-refinement.
- **No ergonomic total order until traits.** `@total_cmp` (§8) gives sorting,
  hashing, and ordered containers a usable total order immediately, but it's an
  intrinsic call site, not `Ord`/operator integration; a `NaN` compared with `<`
  is still unordered until code explicitly opts into `@total_cmp`. Full
  ergonomics wait on RUE-246.
- The register-class refactor touches both backends' hottest code (regalloc,
  scheduling); a subtle bug there is a broad blast radius, which is why Phase 1
  is isolated and validated as a pure no-op first.
- Two float widths double the backend surface (instruction variants, ABI slots,
  conversion matrix) versus an f64-only start.

### Neutral
- `std.math` transcendentals and runtime `strtod` are out of scope; the feature
  ships useful without them and they are additive later.
- `zmij` is already vendored; adopting it adds a runtime symbol but no new
  third-party dependency decision.

## Open Questions

None outstanding. The four questions raised in proposal — total-order timing,
`%` on floats, `@float_to_int` trap-vs-saturate, and `f16`/`f128` scope — were
all ratified on 2026-07-20 and are recorded in Decision §§1, 4, 8, 9.

## Future Work

- Traits (`PartialEq`/`Eq`, `PartialOrd`/`Ord`) once RUE-246 lands; `@total_cmp`
  (§8) becomes the literal implementation of `Ord::cmp` at that point.
- Whether to add a general `@bit_cast` intrinsic (useful beyond floats):
  flagged during ratification as a side question deliberately left unresolved
  by this ADR (§8). Revisit if a concrete need for float→int reinterpretation
  beyond `@total_cmp` appears.
- `std.math.rem` (`fmod`/IEEE remainder) once a concrete use case picks a
  semantics (§9), and `std.math` transcendentals once a libm-class strategy
  exists.
- Runtime `string → float` parsing (`strtod`).
- SIMD / vector float types, building on the register-class infrastructure.
- FP calling-convention completion for C FFI floats (RUE-1059).

## References

- RUE-714 (this design gate); related FFI work RUE-1059, RUE-1054, RUE-742/745.
- [ADR-0025: Compile-Time Execution](0025-comptime.md) — `comptime_float` table entry.
- [ADR-0035: String model](0035-string-model-byte-strings.md) — "lossiness is never implicit".
- [ADR-0036: Behavior classification preference](0036-behavior-classification-preference.md) — most-defined category.
- [ADR-0048: Shared codegen middle layer](0048-shared-codegen-middle-layer.md) — reuse across backends.
- Spec 4.2 (integer division traps) and 4.3 (comparison/equality; the `NaN` forward reference).
- Prior art: [Rust `f64::total_cmp`](https://doc.rust-lang.org/std/primitive.f64.html#method.total_cmp),
  [Zig floats](https://ziglang.org/documentation/master/#Floats),
  [Go constants](https://go.dev/ref/spec#Constants),
  [Swift `FloatingPoint`](https://developer.apple.com/documentation/swift/floatingpoint),
  [Hylo specification](https://hylo-lang.org/docs/reference/specification/),
  C ISO/IEC 9899 §6.3 (usual arithmetic conversions).
