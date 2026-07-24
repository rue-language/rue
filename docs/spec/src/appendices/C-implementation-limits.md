+++
title = "Implementation Limits"
weight = 3
template = "spec/page.html"
+++

# Appendix C: Implementation Limits

{{ rule(id="C.1:1") }}

This appendix documents implementation-defined limits of the Rue compiler. These limits are normative: a conforming implementation **MUST** support at least the minimum values specified here. Implementations **MAY** support larger values.

{{ rule(id="C.1:2") }}

Programs that exceed these limits are not guaranteed to compile or execute correctly. A conforming implementation **SHOULD** produce a diagnostic when a limit is exceeded.

## Numeric Limits

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

{{ rule(id="C.3:1", cat="informative") }}

Source file size **MUST** be representable using 32-bit byte offsets. This limits source files to 4 GiB (4,294,967,295 bytes).

{{ rule(id="C.3:2", cat="informative") }}

The span tracking system uses 32-bit unsigned integers for byte offsets, which determines this limit.

## Array Limits

{{ rule(id="C.4:1", cat="normative") }}

Array length **MUST** be representable as an unsigned 64-bit integer. This limits array sizes to 2^64 - 1 elements.

{{ rule(id="C.4:2", cat="informative") }}

Practical limits on array size are determined by available memory and platform constraints rather than the type system.

{{ rule(id="C.4:3", cat="informative") }}

The current implementation limits any single object (including an array type) to 2,147,483,647 bytes (`i32::MAX`), matching the code generator's frame-offset addressing range. A type whose layout exceeds this limit is rejected with a diagnostic (E0906) wherever a value of the type would be materialized — a variable, a parameter, or a `@size_of`/`@align_of` query.

The cumulative storage for one function is limited to 2,147,483,632 bytes: the
largest 16-byte-aligned value within that same signed displacement range.
Locals, parameter homes, hidden return storage, register-allocation spills, and
the simultaneous outgoing call area all count toward this checked budget.
Exceeding it is rejected with diagnostic E0907.

## Identifier Limits

{{ rule(id="C.5:1", cat="informative") }}

There is no explicit limit on identifier length. Identifiers are stored as dynamically-allocated strings and are limited only by available memory.

## Minimum Guaranteed Limits

{{ rule(id="C.6:1", cat="normative") }}

A conforming implementation **MUST** support at least:

| Construct | Minimum Limit |
|-----------|---------------|
| Source file size | 4 GiB |
| Integer literal value | 2^64 - 1 |
| Array length | 2^64 - 1 |
| Function parameters | No fixed limit |
| Struct fields | No fixed limit |
| Enum variants | 2^64 |
| Syntactic nesting depth (expressions, types, blocks, loops) | 256 levels |

{{ rule(id="C.6:2", cat="informative") }}

"No fixed limit" means the construct is limited only by available memory, not by an explicit cap in the implementation.

{{ rule(id="C.6:3", cat="normative") }}

Syntactic nesting depth — the depth to which expressions, types, and blocks may be nested within one another — is bounded. A conforming implementation **MUST** support a nesting depth of at least 256 levels, and **MUST** diagnose input that exceeds its supported maximum with a clear error rather than exhausting the stack or otherwise failing catastrophically. The reference implementation rejects over-deep input with error `E0482` and a fixed maximum of 256 levels. This bound applies uniformly to every recursive syntactic construct, including parenthesised and operator-chained expressions (`((…))`, `a + a + …`), field and method chains (`a.b.c…`), nested types (`[[…]]`, `ptr const ptr const …`), and `else if` chains.

## Stack and Memory Considerations

{{ rule(id="C.7:1", cat="informative") }}

While the language specification does not impose limits on recursion depth or stack usage, practical execution is constrained by:

- Operating system stack limits
- Available memory for local variables
- Platform-specific calling convention limits

{{ rule(id="C.7:2", cat="informative") }}

Programs requiring deep recursion or large stack allocations **SHOULD** be designed with these platform constraints in mind.

## Code Generation Limits

{{ rule(id="C.8:1", cat="normative") }}

Function size is limited by the target architecture's addressing modes:

- On x86-64, functions **MUST** fit within the ±2 GiB range addressable by 32-bit relative offsets
- Jump instructions within a function use 32-bit relative addressing to support functions of any reasonable size

{{ rule(id="C.8:2", cat="informative") }}

The compiler uses 32-bit relative (rel32) encoding for all conditional and unconditional jumps, avoiding the 127-byte limit of 8-bit relative (rel8) encoding. This ensures functions with large basic blocks compile correctly without requiring multi-pass relaxation.
