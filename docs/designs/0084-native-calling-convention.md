---
id: 0084
title: "The native Rue calling convention: the target C convention plus a wider return bank"
status: accepted
tags: [abi, codegen, semantics]
feature-flag: (none - an internal ABI change needs no preview gate)
created: 2026-09-05
accepted: 2026-09-05
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-2030", "RUE-2037", "RUE-2038", "RUE-2039", "RUE-2010", "RUE-2046", "ADR-0052", "ADR-0064", "ADR-0055"]
---

# ADR-0084: The native Rue calling convention is the target C convention plus a wider return bank

## Status

Accepted 2026-09-05. Steve approved the direction on 2026-09-04 when he approved
the RUE-2030 epic; this record documents that decision and the phases that carry
it out, and does not reopen it.

The infrastructure the decision stands on has already landed under RUE-2030:
`rue_target::CallingConvention` with concrete rows and `"C"` as a target-resolved
alias, `CConventionSpec` describing each C row as data
(`crates/rue-target/src/calling_convention.rs`), `rue_air::lower_c_signature` and
its `LoweredSignature` (`crates/rue-air/src/lowered_signature.rs`) consumed by
every C crossing, export thunks generated from that signature
(`crates/rue-codegen/src/export_thunk.rs`), `--emit abi`, explicit ABI strings
([ADR-0064](0064-c-ffi.md) Amendment 2), and a generated C-boundary conformance
matrix (`crates/rue-c-abi-matrix`) that runs on all three native CI hosts. What
remains is to point the *native* convention at the same machinery.

## Summary

The native `Rue` convention becomes **the compilation target's C convention with
one deliberate difference: a return-register bank wider than C's, ordered so
C's own return registers are a prefix of it.** Arguments are placed exactly as
the target's C row places them — SysV eightbyte classification on x86-64,
AAPCS64 composite rules on AArch64, Apple's amendments on the Darwin row — and
that placement is computed by `rue_air::lower_c_signature`, the one function
every C crossing already consumes. The convention keeps Rue's canonical
64-bit-extension invariant, which is stronger than any C row requires, and keeps
its existing rules for zero-sized values and `borrow`/`inout` parameters. It
remains unspecified and free to change between compiler revisions; what changes
is that it stops being an *independent* convention and becomes a named extension
of one the compiler already implements and tests. This is the shape Swift's
`swiftcc` has: the platform convention plus what the language needs on top of it.

## Context

### Today's native convention

The native convention is classified by `NativeCallAbi` / `NativeAbiTypeFacts`
(`crates/rue-air/src/call_abi.rs`) and planned by
`crates/rue-codegen/src/call_plan.rs`, with the callee half in
`crates/rue-cfg/src/build.rs`. Its rules:

- A value is flattened into logical 8-byte ABI slots: **one slot per scalar
  leaf, whatever its width**, one slot per struct field and per array element.
- **A multi-slot aggregate's slots are reversed** before placement — at the
  caller in `call_plan.rs`, and again in the callee's parameter contract in
  `rue-cfg/src/build.rs`. The reversal exists so that the overflow slots of a
  value that does not fit in registers land in *ascending* frame order; the
  callee reconstructs logical field order.
- The return bank is **six general-purpose registers on x86-64**
  (`rax`, `rdx`, `rcx`, `r8`, `r9`, `r10`) and **eight on AArch64** (`x0`-`x7`),
  plus **eight floating-point registers on each** (`xmm0`-`xmm7`, `v0`-`v7`).
- `sret` is a **hidden first ordinary argument slot** — `rdi` on x86-64 with no
  `rax` echo, `x0` on AArch64 rather than the dedicated `x8`.
- `StrBuf` always returns through sret; zero-sized values are omitted;
  `borrow`/`inout` is one pointer; every scalar is held in canonical
  64-bit-extended form in registers and native slots.
- Transitionally, under the compact layout, an aggregate of more than one slot
  whose compact image is not slot-identical is forced indirect — ADR-0052's
  ratified ruling 9, the "memory-first" rule that let memory layout migrate
  while the call convention stood still.

`docs/notes/ffi-abi-conformance-audit.md` tabulates all of this in its "Current
convention matrix" and describes it in "Native Rue convention".

### What it costs

- **Register use disproportionate to the value.** A `[u8; 16]` argument occupies
  sixteen native slots where C uses two eightbytes; a four-field `u8` struct
  takes four registers where C takes one. The slot model spends a register per
  leaf and then spills.
- **Classification lives in more than one place.** Argument slot classes are
  computed in `call_plan.rs` and `rue-cfg/src/build.rs`; return transport is
  decided separately in the return plan. RUE-2010 is the symptom: an aggregate
  argument slot holding a float is already classed `Fp` and travels in
  `xmm0`/`v0`, while the same shape returned by value is transported through the
  general-purpose return registers, and the two halves ICE against each other.
- **Exports need a marshaling thunk.** A `pub extern "C" fn` is an ordinary
  native body under a mangled symbol plus a second object that adapts C's
  placement to the native one — including reversing a multi-slot aggregate's
  slots — because the two conventions genuinely disagree about a two-field
  struct.

None of this is forced. The compiler now has a complete, data-driven,
execution-verified implementation of three C conventions sitting beside the
native one; the native convention's differences from it are historical, not
designed.

## Decision

### 1. The native convention is unspecified, and pinned at each revision

`CallingConvention::Rue` is **unspecified and free to change between compiler
revisions**, in the way LLVM's `fastcc`, Rust's `"Rust"`, and Zig's `.auto` are.
Separately compiled objects from different compiler revisions are not promised
to interoperate across it, and no source-level guarantee depends on where a
native argument travels. `"rue"` is not an `extern` ABI string (ADR-0064
Amendment 2): naming the native convention at a foreign boundary would describe
no crossing.

Unspecified is not the same as undescribed. At any given revision the convention
is pinned by executable artifacts, so a change to it is deliberate and reviewed
rather than discovered:

- the oracle's call-contract model (`crates/rue-oracle`, whose native crossing
  rules are exercised by `crates/rue-oracle/src/tests/call_contracts.rs`) and the
  oracle-diff corpora that compare it against the compiler;
- the native aggregate matrices —
  `crates/rue-cli-tests/cases/aggregate_abi_matrix.toml`,
  `crates/rue-cli-tests/cases/abi.toml`, and
  `crates/rue-cli-tests/cases/abi_conformance.toml` — which compile, link, and
  run probes on each native CI host; and
- `--emit abi`, which prints the convention a signature follows and where every
  parameter and result actually travels.

The language specification does not describe the native convention and gains no
description of it here. Its only contact with the slot model is Appendix C's
implementation limits, which §4 addresses.

### 2. The convention: the target C convention plus a wider return bank

#### Arguments are placed exactly as the target's C row places them

There is no native argument rule left over. For the compilation target's C row:

- **x86-64 SysV**: eightbyte classification with the INTEGER and SSE classes,
  aggregates of at most two eightbytes in registers of the classified banks,
  MEMORY-class aggregates byval in the outgoing argument area consuming no
  register.
- **AAPCS64**: the composite rules, including rule C.11's roster exhaustion (a
  composite that does not fit the remaining registers stacks *and* exhausts the
  bank, so later arguments stack too) and by-reference caller-owned copies for
  composites over 16 bytes.
- **Apple arm64 (`Aarch64AapcsDarwin`)**: AAPCS64 with Apple's amendments —
  stacked scalars packed at natural size and alignment, and the caller extending
  arguments narrower than 32 bits.
- Narrow integers, `bool`, and pointers take one register each; past the
  register budget, the convention's own stack packing applies.

This applies to **every Rue type by its compact physical layout** (ADR-0052),
including enums, arrays, and unmarked structs — not only to `@repr(c)` types.
`@repr(c)` remains what ADR-0064 Amendment 1 made it: the *guarantee* that a
type's layout equals the target C aggregate's, and the gate on which types may
appear in an `extern` signature. It is not a precondition for classification.
The native convention classifies whatever layout the type actually has; whether
that layout is promised to match a C compiler's is a separate question, and the
answer for an unmarked type is still no.

Two implementation consequences follow from adopting the C rules natively, both
owned by phase 1:

- The native side is the **first consumer to reach the SSE half of the
  classifier**. `f32`/`f64` exist natively (ADR-0065) but are still rejected at
  the C boundary (ADR-0064 P5, RUE-714), so `EightbyteClass::Sse`,
  `CRegisterClass::Fp`, and AAPCS64's homogeneous-float-aggregate rules are
  carried in the data but unreached by `lower_c_signature` today —
  `EightbyteClasses::all_integer` is its only constructor. Phase 1 implements
  them for the native convention; the C boundary inherits them when RUE-714
  unblocks ADR-0064's float phase.
- `EightbyteClasses` currently holds at most two eightbytes, which is all a C
  row's register-passed aggregate can span. The wider return bank below needs a
  wider classification, which phase 2 owns.

#### The return bank is wider than C's, and C's bank is its prefix

C's return bank is too small for the way Rue code is written: `Result`, `Option`,
and small structs come back from every other call, and two registers force a
memory round trip on shapes that easily fit in registers. The native convention
therefore keeps a wider bank.

**Decision: keep the current bank sizes** — six general-purpose registers on
x86-64, eight on AArch64, and eight floating-point registers on each — **ordered
so that the C row's own return registers are a prefix**: `rax`, `rdx`, … and
`xmm0`, `xmm1`, … on SysV; `x0`, `x1`, … and `v0`-`v3`, … on AAPCS64. The
reasoning:

- The sizes are already implemented, allocated around, and executed on both
  backends and all three native CI hosts, so keeping them costs nothing and
  changing them would be a migration inside a migration.
- Every register beyond C's own return registers is caller-saved in every C row
  Rue supports (`rcx`, `r8`, `r9`, `r10`, `xmm2`-`xmm7`; `x2`-`x7`, `v4`-`v7`),
  so a wider bank imposes no new save/restore obligation at a call site.
- Prefix ordering means **any value that fits C's bank returns exactly as C
  would**. That is what lets an export be an alias rather than a thunk, and it
  keeps the difference between the two conventions to a single, stateable
  sentence.

An aggregate whose eightbyte count fits the bank returns in registers **by
eightbyte class in ascending memory order — no reversal**. Otherwise it returns
through the **C row's own indirect-result register**: `rdi` with the `rax` echo
on SysV AMD64, the dedicated `x8` on AAPCS64. The native
hidden-first-ordinary-argument sret, with its missing echo and its unused `x8`,
retires.

**Alternative considered: four GP plus four FP** — Swift's `swiftcc` number. It
is a defensible bank: four registers cover the overwhelming majority of returned
aggregates, and a smaller bank leaves more registers unconstrained at a call
boundary. It was rejected because the argument for it is theoretical while the
argument against it is concrete: the six/eight-register bank exists, is tested,
and its extra registers are caller-saved anyway, so narrowing it would spend a
codegen campaign to make some programs slower and none faster. The number is not
load-bearing for anything else in this design — it is a field of the convention
description — so revisiting it later is an edit, not a redesign.

#### The canonical-extension invariant stays

Every scalar is held **64-bit-extended** in registers and native slots. This is
stronger than any C row requires: SysV leaves the bits above a narrow argument's
declared width unspecified, AAPCS64 defines only bits 0..31, and Apple's row
requires the caller to extend below 32 bits. A native value therefore already
satisfies every row on the way out.

The consequence for exports is precise, and it is the reason exports get
cheaper:

- A `pub extern "C" fn` whose **parameters are all register-width scalars,
  pointers, or aggregates** — that is, none of them a scalar narrower than a
  register — **and whose result is within C's own return bank** needs no thunk at
  all. Both conventions place every value identically, so the C symbol is emitted
  as an **alias of the native body** rather than as a separate object that
  marshals into it.
- An export with a **narrow scalar parameter** keeps a prologue that
  **re-extends** it. A C caller does not promise the high bits (except on the
  Apple row, and Rue does not rely on one row's guarantee for a rule that must
  hold on all of them), while the native body requires the canonical form. That
  prologue is the whole of the remaining thunk: a re-extension per narrow
  parameter, not a marshaling pass.
- An export whose **result exceeds C's return bank but fits Rue's** also keeps a
  thunk, because that is exactly where the two conventions differ.

Symmetrically, a *native* caller of a C function re-extends a narrow result on
return, which is the `ScalarAbiExtension` the lowered signature already carries.

#### Everything else keeps its current rule

- **Zero-sized values are omitted** — no register, no stack byte, no pointer.
- **`borrow` and `inout` are one pointer**, whatever they point at.
- **`StrBuf` has no special rule.** Its always-sret special case goes: it is a
  24-byte three-field aggregate and takes whatever the C row gives a 24-byte
  struct — byval in the argument area under SysV, a by-reference caller-owned
  copy under AAPCS64, and an indirect result under both. The special case exists
  today because the slot model had no general answer; the C rules do.
- **Runtime helpers are unchanged.** `rue-runtime-abi` helpers (ADR-0055) are C
  calls and already follow the target's C row through the same `"C"` alias table.
  Nothing in this ADR changes their placement; what changes is that the native
  path they are contrasted with stops being a different algorithm.

### 3. Reserved extensions, named and not designed

Two extensions are compatible with a C-plus convention and are worth naming so
that later work is recognized as additive rather than as a second redesign.
Neither is designed here, and neither is in scope for the phases below; each
would be its own ADR.

- **A dedicated error register for `Result`-returning functions**, in the shape
  of Swift's `swifterror`: a callee-preserved register carrying the error half of
  a `Result` so the common success path does not pay for the sum type's width.
  ADR-0038 makes `Result` the error-handling surface, so this is the extension
  with the most obvious payoff.
- **`preserve_most`-style clobber sets for trap and cold runtime paths**: a
  convention variant in which the callee preserves nearly every register, so a
  cold call — a trap report, a slow-path runtime helper — does not force the hot
  caller to spill around it.

Both are additive over the C-plus convention: a register the native convention
does not otherwise use, and a clobber set attached to a convention row.

### 4. What retires

- **The one-slot-per-leaf decomposition** as the native argument model.
- **The reversal**, at both the caller (`crates/rue-codegen/src/call_plan.rs`)
  and the callee (`crates/rue-cfg/src/build.rs`), *and the reason it existed*:
  ascending frame order for overflow slots is a property the C rows' own stack
  packing already has, so nothing is left to compensate for.
- **The memory-first indirectness rule** (ADR-0052 ruling 9): an aggregate whose
  compact image is not slot-identical no longer needs to be forced indirect,
  because the convention classifies the compact image directly. The rule was
  explicitly transitional — it existed to let layout migrate while the convention
  stood still — and this is the change it was waiting for.
- **The native/runtime split in call planning**: `callee_convention` and the
  `CallTarget::Rue` versus `CallTarget::MemoryBuiltin` branch in `call_plan.rs`,
  and the parallel `RuntimeCallPlan` path. (The epic calls this the `CalleeAbi`
  split; there is no type of that name in the tree today, and these are the sites
  it names.)
- **The CFG carrying ABI slot classes**: the `NativeArgClass` metadata and the
  slot-oriented parameter contract in `crates/rue-cfg/src/build.rs` and its
  verifier. After the phases the CFG describes a parameter by type and mode, and
  placement is a codegen-side query against the lowered signature.
- **The export thunk as a separate marshaling path**, reduced to the narrow
  re-extension prologue described above, or to nothing.
- **The separate native return classifier.** Returns and arguments are classified
  by the same function against the same leaf mapping, which is what closes
  RUE-2010 by construction.

#### `abi_slot_count` and Appendix C.4

`abi_slot_count` (`crates/rue-air/src/intern_pool.rs`) survives, but **only as a
layout measure**, not as a description of any crossing. Appendix C.4's
object-size ceiling (E0906) is spelled in those slots: C.4:3 limits an object to
268,435,455 ABI slots, derived from the code generator's signed 32-bit
frame-displacement range divided by the 8-byte slot width, and C.4:2 explains the
count as "one 8-byte slot per scalar, per struct field, and per array element".

**The limit's wording does need to change**, and the change is deferred to phase
3. The ceiling itself does not move: it is a frame-addressing bound, it is
independent of how a value crosses a call, and changing which programs it accepts
would be a language-visible change this ADR does not make. What becomes wrong is
the *name and the justification*: once no call decomposes a value into one slot
per leaf, "ABI slot" names a measure with no ABI in it, and a reader who follows
the term to the calling convention will find nothing that matches. Phase 3, which
is where the documentation catches up with the architecture, decides whether to
rename the measure, restate the ceiling's derivation without the word, or leave
the spelling and correct the prose — and lands that answer in
`docs/spec/src/appendices/C-implementation-limits.md` with its traceability.

## Implementation Phases

Each phase is an existing Linear issue and lands as its own PR. Each is
**execution-verified** on both backends together, by the native aggregate
matrices (`crates/rue-cli-tests/cases/aggregate_abi_matrix.toml`,
`crates/rue-cli-tests/cases/abi.toml`,
`crates/rue-cli-tests/cases/abi_conformance.toml`), the generated C matrix
(`//crates/rue-c-abi-matrix:c-abi-matrix-test`), and the oracle-diff corpora
(`//crates/rue-oracle-diff:oracle-diff-test`). The oracle's call-contract model
moves in the same change as the convention it models, never in a follow-up.

- [ ] **Phase 1: Native argument classification adopts the C row's placement** —
  RUE-2037. By-value aggregates pack into eightbytes in ascending memory order
  (SysV) or follow the AAPCS64 composite rules; the reversal retires at both
  ends, along with the drop-glue and value-plan reversals that mirror it; narrow
  scalars keep the canonical 64-bit form; arguments past the register budget
  follow the row's stack packing; `borrow`/`inout` stays one pointer and
  zero-sized stays omitted. Returns are untouched. RUE-2046 — routing
  scalars-only `extern "C"` calls through the lowering, the last C crossing that
  still takes the native path — is a preliminary of this phase and may land
  earlier.
- [ ] **Phase 2: Native returns through the shared lowering** — RUE-2038. Returns
  classify through the same lowered signature, with the wider bank and the psABI
  sret register and echo; the hidden-first-ordinary-argument sret and the
  `StrBuf` special case retire; RUE-2010 closes by construction, with its
  preview-gated spec cases landing here; export thunks reduce to aliases wherever
  the convention allows.
- [ ] **Phase 3: One classifier, and the docs describe it** — RUE-2039. The CFG
  parameter contract drops `NativeArgClass` and the slot-oriented description;
  the native/runtime/C branching in call planning and both backends' CFG lowering
  collapses to one classifier; the runtime helper path consumes it as a C call
  with the manifest signature as its type facts; `docs/runtime-abi.md`,
  `docs/architecture.md`, and `docs/notes/ffi-abi-conformance-audit.md` describe
  the present architecture with no old-versus-new narration, and the Appendix
  C.4 wording question above is answered. RUE-2030 closes with the matrices green
  on all three native hosts.

## Consequences

### Positive

- **One classifier for every crossing.** Calls, returns, exports, runtime
  helpers, and — when they exist — callbacks consume `lower_c_signature`, so a
  RUE-2010-class disagreement between two halves of the ABI becomes impossible
  rather than merely fixed.
- **Register use proportional to C's.** A `[u8; 16]` argument crosses in two
  registers instead of sixteen slots; a four-field `u8` struct in one.
- **Thunk-free exports.** A `pub extern "C" fn` whose signature places
  identically under both conventions is an alias of its native body; what remains
  of the thunk is a re-extension per narrow parameter.
- **The Apple row is exercised natively.** Darwin's amendments stop being
  reachable only through `extern "C"` and apply to ordinary Rue code, so the row
  gets the same execution coverage the rest of the compiler has.
- **The convention is describable as data.** It is the target's C row plus two
  raised return-register counts, so a new target is a new row rather than a new
  algorithm.

### Negative

- **A codegen campaign on the scale of the ADR-0052 layout migration.** It
  touches both backends, the CFG parameter contract, drop glue, the export thunk,
  and the oracle's call-contract model, and every phase must land on both
  architectures together (AGENTS.md multi-backend rule).
- **Phase 1 must implement classifier surface the C boundary never reached**: the
  SSE eightbyte class and AAPCS64's homogeneous-float-aggregate rules, which
  exist as data but are unreachable today because the C boundary rejects floats.
- **The oracle moves with each phase.** The call-contract model is a second
  implementation of the convention by design; keeping the oracle-diff corpora
  green is part of each phase's cost, not a follow-up.
- **A performance regression is possible in the shapes that lose registers.**
  Register use becomes proportional to C's in both directions: a 24-byte
  three-scalar aggregate that occupies three registers under the native slot
  bank today is MEMORY-class byval on SysV and a caller-owned copy under
  AAPCS64. The matrices prove correctness, not speed.

### Neutral

- **The unstable ABI stays unstable.** Nothing here promises cross-revision
  interoperability, and the pinning artifacts of §1 are unchanged in kind.
- **No source-language change.** No syntax, no semantics, no diagnostics; a
  program's meaning is identical before and after.
- **No preview gate.** ADR-0005 gates *language* surface. This changes where
  values travel inside a compilation, which no program can observe except through
  `--emit abi`, so there is nothing to gate and nothing to stabilize.
- **`@repr(c)` is unchanged.** It still guarantees C layout and still gates
  `extern` signatures; it simply was never what decided native placement.

## Future Work

- **A dedicated error register for `Result` returns** (`swifterror`-shaped) — §3.
- **`preserve_most`-style clobber sets** for trap and cold runtime paths — §3.
- **Callback trampolines**, the fourth consumer of the same lowered signature.
- **Floating-point at the C boundary** (ADR-0064 P5, RUE-714), which inherits the
  SSE classification phase 1 implements for the native side.
- **Apple's byte-exact placement of a stacked composite argument**, still open in
  `docs/notes/ffi-abi-conformance-audit.md` and needing byte-granular marshaling
  on both conventions alike.

## References

- RUE-2030 — the calling-convention epic this ADR is slice 6 of; RUE-2037,
  RUE-2038, RUE-2039 are its phases and RUE-2046 a preliminary.
- RUE-2010 — the single-slot float-carrying aggregate return that phase 2 closes
  by construction.
- [ADR-0064](0064-c-ffi.md) — the C boundary, its `@repr(c)` representation
  guarantee (Amendment 1), and named ABI strings (Amendment 2).
- [ADR-0052](0052-canonical-physical-type-layout.md) — canonical physical layout,
  the three independent representations, and the memory-first transitional ruling
  this ADR retires.
- [ADR-0055](0055-typed-runtime-abi-manifest.md) — the typed runtime ABI manifest
  whose helpers are C calls and are unaffected.
- [ADR-0065](0065-floating-point.md) — the float types and register classes the
  native convention must classify.
- `docs/runtime-abi.md` — the convention table, `CConventionSpec`, and the one
  signature lowering.
- `docs/notes/ffi-abi-conformance-audit.md` — the audit whose "Native Rue
  convention" section describes the convention this ADR replaces.
- `crates/rue-target/src/calling_convention.rs` — conventions as data.
  `crates/rue-air/src/lowered_signature.rs` — the one placement function.
  `crates/rue-air/src/call_abi.rs` — the native classifier being retired.
