+++
title = "Conformance, Undefined Behavior, and Runtime Panics"
weight = 2
template = "spec/page.html"
+++

# Appendix B: Conformance, Undefined Behavior, and Runtime Panics

This appendix is a consolidated reference for how Rue classifies program
behavior. It restates the conformance taxonomy introduced in §1.3, catalogues
the concrete conditions Rue **traps** as defined runtime panics, and enumerates
the narrow set of operations that are genuinely **undefined**. It is normative
only where a rule carries a normative category (see B.1:5); the catalogues
themselves are informative summaries whose normative homes are the chapters they
cross-reference.

## Behavior Classification

{{ rule(id="B.1:1", cat="informative") }}

This specification classifies the behavior of a program into four conformance
categories — **undefined**, **unspecified**, **implementation-defined**, and
**erroneous** — following C, C++, and Rust. The categories are introduced in
§1.3; this appendix collects the concrete instances of each. How a behavior is
assigned to a category is governed by a design preference (prefer the
most-defined category; confine undefined behavior to `unchecked` operations that
cannot be checked otherwise) recorded in ADR-0036, not by a normative rule of
this document.

{{ rule(id="B.1:2", cat="informative") }}

The four behavior categories and the obligations each places on a conforming
implementation:

| Category | Obligation on the implementation |
|----------|----------------------------------|
| **Undefined** | None. A program that exhibits undefined behavior is invalid and the implementation may do anything. In Rue this arises **only within `unchecked` code** (see B.3). |
| **Unspecified** | Choose from a permitted set of behaviors; no particular choice is required and none need be documented. Rue specifies most such choices (e.g. evaluation order §4.0, drop order §3.9), so this category has few instances today. |
| **Implementation-defined** | Choose from a permitted set **and document** the choice. Examples: `StrBuf` growth/capacity (§3.7:41, documented in §3.10:25 — and *observable*, since `capacity()` (§3.10:11) returns the chosen value, so a program that branches on it is not portable), struct and array layout (§3.6), the width of `usize`/`isize` (§3.1), and the limits of Appendix C. |
| **Erroneous** | Well-defined behavior that is nonetheless a program error a conforming implementation is encouraged to diagnose. Rue currently has **no** erroneous behavior — conditions other languages leave erroneous (e.g. integer overflow) Rue instead *traps* as a defined panic (see B.2). |

{{ rule(id="B.1:3", cat="informative") }}

The distinction between **undefined** and **erroneous** is central to Rue's
character: an erroneous operation still has a defined result (and is merely
diagnosable), whereas an undefined operation imposes no requirements at all.
Rue's memory-safety guarantee is precisely that the safe subset of the language
has **no undefined behavior**; every hazard reachable outside a `checked` block
is either rejected at compile time or trapped at runtime (B.2). Undefined
behavior is confined to the `unchecked` operations catalogued in B.3.

{{ rule(id="B.1:4", cat="informative") }}

Separately from the *behavior* categories above, each normative paragraph in
this specification carries a **paragraph category** (the `cat=` marker) that
states what kind of requirement it is. These are defined in §1.3 and summarized
here:

| Paragraph category | Meaning | Normative? |
|--------------------|---------|------------|
| `normative` | A general requirement on a conforming implementation. | Yes |
| `legality-rule` | A compile-time requirement that must be enforced. | Yes |
| `syntax` | A grammar rule defining valid program structure. | Yes |
| `dynamic-semantics` | A runtime-behavior requirement. | Yes |
| `undefined-behavior` | A condition whose behavior is undefined (imposes no requirement, but identifies the hazard). | Yes |
| `informative` | Explanatory text. | No |
| `example` | A code example. | No |

{{ rule(id="B.1:5", cat="informative") }}

The two axes are independent: a paragraph's `cat=` marker classifies the
*paragraph*, while the four behavior categories classify a *program's
behavior*. A `dynamic-semantics` paragraph may, for instance, define a trap
(B.2) or state that an `unchecked` operation is undefined (B.3). Paragraphs
without an explicit `cat=` marker are informative.

## Runtime Panics

{{ rule(id="B.2:1") }}

Rue detects certain error conditions at runtime and responds with a **trap**: a
defined runtime panic that terminates the program with a specific exit code.
These are *defined* behaviors, not undefined behavior — the program's response
is fully specified. This is the mechanism by which Rue keeps conditions other
languages leave undefined (overflow, out-of-bounds access, division by zero)
inside the defined end of the taxonomy (B.1).

### Integer Overflow

{{ rule(id="B.2:2", cat="dynamic-semantics") }}

Signed or unsigned integer arithmetic that overflows the representable range
**MUST** cause a runtime panic (§8.1).

**Operations affected:**
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Unary negation (`-`)
- Division (`/`) and remainder (`%`), **exactly** when the dividend is the
  signed type's minimum value and the divisor is `-1` (the quotient `-MIN` is
  not representable; the remainder operation overflows in the same case even
  though its mathematical result would be `0`). This is distinct from division
  by zero (B.2:3).

**Runtime behavior:** Panic with exit code 101.

### Division by Zero

{{ rule(id="B.2:3", cat="dynamic-semantics") }}

Division or remainder with a divisor of zero **MUST** cause a runtime panic
(§8.3).

**Operations affected:**
- Division (`/`)
- Remainder (`%`)

**Runtime behavior:** Panic with exit code 101.

### Array Bounds Violation

{{ rule(id="B.2:4", cat="dynamic-semantics") }}

Accessing an array element with an index outside the valid range `[0, length)`
**MUST** cause a runtime panic (§8.2).

**Operations affected:**
- Array indexing (`arr[i]`)
- Array element assignment (`arr[i] = v`)

**Runtime behavior:** Panic with exit code 101.

### Exit Codes

| Condition | Exit Code |
|-----------|-----------|
| Integer overflow (incl. signed `MIN / -1`) | 101 |
| Division by zero | 101 |
| Array out of bounds | 101 |

{{ rule(id="B.2:5") }}

All runtime panics produce exit code 101, matching Rust's convention for
unwinding panics.

## Undefined Behavior

{{ rule(id="B.3:1", cat="informative") }}

Rue has undefined behavior **only within `unchecked` code** — the raw-pointer
and heap intrinsics of Chapter 9, which may appear only inside a `checked` block
(§9.1). No operation reachable from the safe subset is undefined; safe hazards
are rejected at compile time or trapped (B.2). The design preference confining
undefined behavior to `unchecked` operations that cannot be checked without
changing a value's representation is recorded in ADR-0036.

{{ rule(id="B.3:2", cat="informative") }}

Inside a `checked` block, the following operations have **undefined behavior**.
The compiler does **not** verify these conditions; the programmer is responsible
for upholding them (§9, ADR-0028). Each entry cites the normative rule that
defines it.

| Undefined operation | Normative rule |
|---------------------|----------------|
| Reading (`@ptr_read`) or writing (`@ptr_write`) through a pointer that does not address valid, live storage for the pointee type — including a null pointer, a misaligned address, or memory that was never allocated. | §9.2 (9.2:6b), §9.1, ADR-0028 |
| Reading (`@ptr_read_unaligned`) or writing (`@ptr_write_unaligned`) through a pointer that does not address valid, live storage for the pointee type. Only the alignment obligation is lifted for this pair; every other requirement of the aligned pair still applies. | §9.2 (9.2:14k) |
| Offsetting a pointer (`@ptr_offset`) outside the bounds of the allocation it addresses (a one-past-the-end pointer may be formed but not dereferenced). | §9.2 (9.2:7) |
| Using a pointer after the block it addressed has been released with `@free` (use-after-free), or freeing a block that was not returned by `@alloc`/`@alloc_zeroed`/`@realloc`, or freeing one twice. | §9.2 (9.2:11) |
| Accessing storage through a pointer produced by `@int_to_ptr` from an address that does not name valid, correctly typed, correctly aligned storage. | §9.2 (9.2:6c), ADR-0028 |
| Using a pointer obtained from `@raw`/`@raw_mut`/`@field_ptr` after the value it borrowed has been moved, dropped, or otherwise gone out of scope (a dangling pointer). | ADR-0028 |
| Mutating storage through a `ptr mut T` while another live pointer aliases the same storage in a way the program's reasoning assumes cannot happen (aliasing violation). | ADR-0028 |
| Accessing storage through a pointer that does not satisfy the pointee type's alignment requirement, other than through `@ptr_read_unaligned`/`@ptr_write_unaligned`, for which an underaligned address is well defined. | §9.2 (9.2:6b, 9.2:14k), ADR-0028 |
| Reading or writing with `@byte_read`/`@byte_write` when `address_of(p) + offset` is not a live byte within the referenced storage, including null, out-of-bounds, use-after-free, and overflowed-address access. | §9.2 (9.2:14d) |
| Copying with `@byte_copy` between regions that overlap: `[dst, dst + size)` and `[src, src + size)` must be disjoint. `@byte_move` is the well-defined form for overlapping regions. | §9.2 (9.2:14g) |
| Reading or writing outside live storage with `@byte_copy`, `@byte_move`, or `@byte_set` — that is, when `size` reaches past the end of the block either operand addresses. A `size` of zero accesses no memory and is always defined. | §9.2 (9.2:14g) |
| Passing an incorrect size or alignment, a pointer not returned by the allocation family, or an already-freed pointer to `@free`/`@realloc`/`@resize`. | §9.2 (9.2:11–13) |
| Passing an `align` that is zero or not a power of two to the allocation family when the value is not a compile-time constant (a constant one is rejected at compile time instead). | §9.2 (9.2:13a) |

{{ rule(id="B.3:3", cat="informative") }}

These are the memory-safety obligations a `checked` block places on the
programmer (ADR-0028): do not dereference null or dangling pointers, do not
create aliasing violations, respect alignment, keep pointed-to memory valid for
its type, and do not let `@raw`/`@raw_mut` pointers outlive their source. Outside
`checked` blocks, all of Rue's safety guarantees hold and none of the above is
reachable.

{{ rule(id="B.3:4", cat="informative") }}

Undefined behavior is not the same as a trap. A trapped condition (B.2) has a
defined outcome — a panic with exit code 101 — and can be relied upon; an
undefined operation imposes no requirements and may produce any result,
including silent corruption. This is why Rue traps every hazard it can check
cheaply and reserves undefined behavior for the `unchecked` operations where a
check would require changing a value's representation (ADR-0036).
