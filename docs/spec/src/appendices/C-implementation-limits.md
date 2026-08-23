+++
title = "Implementation Limits"
weight = 3
template = "spec/page.html"
+++

# Appendix C: Implementation Limits

{{ rule(id="C.1:1") }}

This appendix documents two different kinds of limit. A **language limit** is part of Rue's semantics: it follows from the language's own types and is the same for every conforming implementation (for example, the range of `i64`). An **implementation limit** is a ceiling of *this* compiler's internal representation: it is implementation-defined (Appendix B), it is derived from a concrete storage decision rather than from the language, and a later release **MAY** raise it. Every implementation limit stated below cites the representation that bounds it.

{{ rule(id="C.1:2", cat="normative") }}

Exceeding an implementation limit is a diagnosable compile-time failure. An implementation **MUST** reject a translation unit that exceeds one of its implementation limits by reporting a diagnostic that names the exceeded limit, and **MUST NOT** instead wrap or truncate an index, silently discard part of the program, exhaust an internal index space, or terminate abnormally. If an insertion fails because the allocator cannot provide memory, the implementation **MUST** report allocation exhaustion (E1402); that diagnostic does not claim that a published limit was reached. This is the general policy; the concrete checks listed in this appendix are instances of it.

{{ rule(id="C.1:3") }}

An implementation **MAY** support values larger than the ones published here, and raising a ceiling is a compatible change. Programs that stay within the *language* limits of §C.2 remain portable; programs that approach an implementation ceiling are relying on an implementation-defined quantity.

## Language Limits

{{ rule(id="C.2:1", cat="normative") }}

Integer literals **MUST** be representable as unsigned 64-bit integers when tokenized. This limits literal values to the range `0` to `18446744073709551615` (2^64 - 1).

{{ rule(id="C.2:2", cat="normative") }}

The following integer types have the specified ranges:

| Type | Minimum | Maximum |
|------|---------|---------|
| `i8` | -128 | 127 |
| `i16` | -32768 | 32767 |
| `i32` | -2147483648 | 2147483647 |
| `i64` | -9223372036854775808 | 9223372036854775807 |
| `u8` | 0 | 255 |
| `u16` | 0 | 65535 |
| `u32` | 0 | 4294967295 |
| `u64` | 0 | 18446744073709551615 |

## Source File Limits

{{ rule(id="C.3:1") }}

A single source file is limited to 4,294,967,295 bytes — one byte short of 4 GiB. Source positions are byte offsets stored as 32-bit unsigned integers, so this is the largest length whose end-of-file position is still representable. The compiler checks the length of every source it accepts and rejects an oversized one with a resource-limit diagnostic (E1401) before lexing, so no span can be formed from a truncated offset.

{{ rule(id="C.3:2") }}

The span representation that bounds it is `rue_span::Span`: a file identifier plus a `u32` start offset and a `u32` end offset. The file identifier is itself a `u32` (`rue_span::FileId`), with `FileId(0)` reserved for the default/unknown file, so a single compilation can distinguish at most 4,294,967,295 source files. Import discovery numbers the files it reaches densely from `FileId(1)` and rejects a larger compilation with a resource-limit diagnostic (E1401) before the count is narrowed to a `u32`, so two sources can never receive the same identifier.

## Array Limits

{{ rule(id="C.4:1", cat="normative") }}

An array length is a compile-time value of type `u64`, so the language itself admits lengths in the range `0` to `18446744073709551615` (2^64 - 1). The object-size ceiling of C.4:3 applies independently and is what a program actually encounters.

{{ rule(id="C.4:2") }}

An array whose element count is legal for the type system may still be rejected: the binding constraint is the number of ABI slots the array type's layout occupies (C.4:3), not available memory. A layout spends one 8-byte slot per scalar, per struct field, and per array element, *whatever the element's own width* — so for an array the slot count is exactly the element count times the element type's own slot count, and for an array of scalars it is exactly the element count. A narrow element type therefore buys no extra headroom: `[i8; N]` and `[i64; N]` reach the ceiling at the same `N`.

{{ rule(id="C.4:3") }}

The current implementation limits any single object (including an array type) to 268,435,455 ABI slots. That ceiling is the code generator's frame-offset addressing range (`i32::MAX`, 2,147,483,647 bytes) divided by the 8-byte slot width, so a layout that fits it is always addressable by a signed 32-bit displacement. A type whose layout needs more slots is rejected with a diagnostic (E0906) wherever a value of the type would be materialized — a variable, a parameter, or a `@size_of`/`@align_of` query — and the diagnostic names the slot ceiling, as C.1:2 requires.

Because the check counts slots rather than bytes, it binds well below 2,147,483,647 bytes for any element narrower than 8 bytes: `[i8; 268435455]` is accepted and reports `@size_of` of 268,435,455 (about 256 MiB), while `[i8; 268435456]` — one element more, and still only about 256 MiB — is rejected. Only an object built entirely from 8-byte scalars reaches the ceiling and the byte range together.

The cumulative storage for one function is limited to 2,147,483,632 bytes: the
largest 16-byte-aligned value within that same signed displacement range.
Locals, parameter homes, hidden return storage, register-allocation spills, and
the simultaneous outgoing call area all count toward this checked budget.
Exceeding it is rejected with diagnostic E0907.

## Identifier Limits

{{ rule(id="C.5:1") }}

There is no separate cap on the length of one identifier: an identifier is a
token, so its length is bounded by the source-file limit of C.3:1. Each
compilation-owned symbol domain uses a string interner whose handles are
non-zero 32-bit keys, so one such domain admits at most 4,294,967,295 distinct
spellings.

The lexer and parser share one per-file staging interner: the parser consumes
the interner produced by lexing. Canonical lowering remaps each module's
vocabulary, parser primitives, and generated RIR spellings into its canonical
destination. Later semantic analysis uses a separate revision-shared equality
domain for AIR symbols, including generated member, specialization, and
anonymous-nominal names. Body-local AIR import and CFG projection may use
additional request-owned domains. These are distinct index spaces: their counts
are not added together, and a `Spur` must never cross domains without
translation. Every reachable insertion in these canonical compilation domains
is fallible; compatibility-only body fixtures retain a private, unbounded
interner and are not part of the canonical query path. Key-space exhaustion is
classified as E1401, while an allocator failure is classified as E1402. The
per-domain ceilings are published in the C.6 table; E1402 does not claim that a
ceiling was reached.

The later body-local AIR import and CFG projection tables are also distinct,
request-owned domains. They are populated only from already admitted canonical
facts and their insertions are checked at the materialization/projection
boundary; an interner failure there follows the same E1401/E1402 policy. These
domains do not extend the revision-shared equality table, and a body-local
`Spur` is never used as a canonical or revision-shared key.

## Implementation Capacity Limits

{{ rule(id="C.6:1") }}

The compiler stores syntax, untyped IR, and typed IR in compact index-based form: instructions are `u32` indices, and the variable-length operands of an instruction (its parameters, fields, variants, arguments, or elements) are `(start: u32, extent: u32)` ranges into one shared word store per program. Every capacity below is a consequence of that representation, not of the language:

| Construct | Limit | Bounded by | Diagnosed by |
|-----------|-------|------------|--------------|
| Source bytes in one file | 4,294,967,295 | `u32` span offsets | E1401, before lexing |
| Source files in one compilation | 4,294,967,295 | `u32` file identifier, `FileId(0)` reserved | E1401, at snapshot assembly |
| Distinct spellings in one per-file lexer/parser staging domain | 4,294,967,295 | non-zero `u32` interner keys | E1401/E1402, during staging |
| Distinct spellings in one canonical semantic symbol domain | 4,294,967,295 | non-zero `u32` interner keys | E1401/E1402, during canonical remapping |
| Distinct symbols in one revision-shared AIR equality domain | 4,294,967,295 | non-zero `u32` interner keys | E1401/E1402, at semantic analysis |
| Distinct symbols in one body-local AIR import or CFG domain | 4,294,967,295 | non-zero `u32` interner keys | E1401/E1402, at materialization/projection |
| IR instructions in one program | 4,294,967,295 | `u32` instruction reference, `u32::MAX` reserved as the null payload | E1401, at RIR publication |
| IR payload words in one program | 4,294,967,295 words (16 GiB) | `u32` payload `start`/`extent` into one word store | E1401, at RIR/CFG payload staging |
| Typed-IR instructions in one function body | 4,294,967,295 | `u32` instruction reference into that body's own array | E1401, at the semantic AIR boundary |
| CFG basic blocks in one function | 4,294,967,295 | `u32` block identifier into that function's own graph | E1401, at CFG construction and optimization |
| CFG values in one function | 4,294,967,295 | `u32` value reference into that function's own graph | E1401, at CFG construction and optimization |
| Parameters of one function | 613,566,756 | 7 payload words per parameter | E1401, via the shared word store |
| Fields of one struct | 2,147,483,647 | 2 payload words per field | E1401, via the shared word store |
| Arguments of one call | 2,147,483,647 | 2 payload words per argument | E1401, via the shared word store |
| Variants of one enum | 4,294,967,295 | 1 payload word per variant; the discriminant tag widens to at most `u32` | E1401, via the shared word store |
| Elements of one array literal | 4,294,967,295 | 1 payload word per element | E1401, via the shared word store |
| Distinct composite types (structs, enums, arrays, pointers, modules) | 16,777,216 | `Type` is a `u32`: an 8-bit kind tag plus a 24-bit type-pool index | E1401, at the semantic boundary |
| ABI slots in one object's layout | 268,435,455 slots | signed 32-bit frame displacement divided by the 8-byte slot width; one slot per scalar, struct field, and array element (C.4:2) | E0906 |
| Cumulative storage of one function | 2,147,483,632 bytes | signed 32-bit frame displacement, 16-byte aligned | E0907 |
| Syntactic nesting depth | 256 | guarded recursion depth in the parser and RIR lowering | E0482 |

{{ rule(id="C.6:2") }}

The ceilings are not independent: parameters, fields, variants, arguments, and array elements all draw on the same per-program word store, so the sum of every payload in a program cannot exceed 4,294,967,295 words even when no individual construct does. That shared store is also what diagnoses them — a payload range that no longer fits `(start: u32, extent: u32)` is rejected when it is staged, whichever construct requested it.

The "Diagnosed by" column names where each check runs, because the compact stores are filled by construction paths that cannot themselves fail. Instructions, composite types, and the per-function CFG arenas are such paths: `add_inst`, `new_block`, and type interning are called from hundreds of infallible sites, so instead of returning an error at each one, the owner records that its ceiling was reached, stops growing, and the next construction, semantic, or optimization boundary converts that record into the E1401 diagnostic. No index is ever wrapped, no entry is ever silently dropped in a compilation that goes on to be published, and no artifact built past a ceiling reaches code generation.

{{ rule(id="C.6:3", cat="normative") }}

Syntactic nesting depth — the depth to which expressions, types, and blocks may be nested within one another — is bounded. A conforming implementation **MUST** support a nesting depth of at least 256 levels, and **MUST** diagnose input that exceeds its supported maximum with a clear error rather than exhausting the stack or otherwise failing catastrophically. The reference implementation rejects over-deep input with error `E0482` and a fixed maximum of 256 levels. This bound applies uniformly to every recursive syntactic construct, including parenthesised and operator-chained expressions (`((…))`, `a + a + …`), field and method chains (`a.b.c…`), nested types (`[[…]]`, `ptr const ptr const …`), and `else if` chains.

{{ rule(id="C.6:4") }}

The rows differ in what they count, and the "Construct" column says which. A per-program row names a store the whole compilation shares, so every construct in the program draws on the same budget. A per-function row — the typed-IR instruction array, the CFG block arena, the CFG value arena, and the frame budget of C.4:3 — names storage that belongs to one function and is indexed only by that function's own identifiers. A per-function ceiling binds independently of program size: a program of any legal size may hold any number of functions that each stay inside it, and one function that exceeds it is rejected even in an otherwise tiny program. The per-construct rows (parameters, fields, arguments, variants, array elements) are neither: they are consequences of the shared per-program word store, as C.6:2 states.

{{ rule(id="C.6:5") }}

A per-function CFG ceiling is checked rather than argued unreachable, because the number of CFG entities a function produces is not a small constant multiple of the typed-IR instructions it was lowered from. Drop elaboration re-emits the pending drops at *every* exit: a `return` emits one drop for each live binding still owning a value, plus a guard block for each binding whose move is path-dependent. A body with `N` droppable bindings and `M` `return` statements therefore lowers to on the order of `N * M` CFG values and blocks, from a body whose own instruction count is on the order of `N + M`.

That expansion is quadratic, so no linear bound on CFG size follows from the ceilings above. Taking `N = M = 65,536` gives 2^32 drop values — past the `u32` value space — from roughly 65,536 bindings of about 32 source bytes each and 65,536 returns of about 16 bytes each: about 3 MiB of source, three orders of magnitude inside the file ceiling of C.3:1, and a typed-IR body four orders of magnitude inside the per-program instruction ceiling. The compiler checks these two ceilings for that reason, and reports E1401 naming the exceeded one.

## Stack and Memory Considerations

{{ rule(id="C.7:1") }}

While the language specification does not impose limits on recursion depth or stack usage, practical execution is constrained by:

- Operating system stack limits
- Available memory for local variables
- Platform-specific calling convention limits

{{ rule(id="C.7:2") }}

Programs requiring deep recursion or large stack allocations **SHOULD** be designed with these platform constraints in mind.

## Code Generation Limits

{{ rule(id="C.8:1", cat="normative") }}

Function size is limited by the target architecture's addressing modes:

- On x86-64, functions **MUST** fit within the ±2 GiB range addressable by 32-bit relative offsets
- Jump instructions within a function use 32-bit relative addressing to support functions of any reasonable size

{{ rule(id="C.8:2") }}

The compiler uses 32-bit relative (rel32) encoding for all conditional and unconditional jumps, avoiding the 127-byte limit of 8-bit relative (rel8) encoding. This ensures functions with large basic blocks compile correctly without requiring multi-pass relaxation.
