+++
title = "Foreign-Boundary Semantics"
weight = 3
template = "spec/page.html"
+++

# Foreign-Boundary Semantics

This section defines the dynamic semantics of the target-C foreign boundary
introduced by ADR-0064 (the `c_ffi` preview feature): calling a C function from
Rue (`extern "C"` imports) and exposing a Rue function to C callers
(`pub extern "C" fn` exports). It complements the calling-convention and
FFI-safety rules of ADR-0064 with the trap, unwinding, and ownership contract at
the boundary, carried from the RUE-740 conformance requirements.

{{ rule(id="9.3:1", cat="informative") }}

A foreign call and a foreign export cross between Rue's convention and the
platform C ABI. Both directions are permitted only in unchecked context (a
foreign *call* requires a `checked` block, § 9.1); an *export* body is ordinary
Rue code. The rules below govern what happens when control, a trap, or ownership
crosses this boundary.

## Abort at the boundary

{{ rule(id="9.3:2", cat="dynamic-semantics") }}

When execution under a C caller — the body of a `pub extern "C" fn` export, or any
Rue call frame reached transitively from such an export — would trap (arithmetic
overflow § 8.1, a failed bounds check § 8.2, division by zero § 8.3, an explicit
`@panic`, or any other abort-class failure), the implementation terminates the
process deterministically at the boundary using the runtime's trap status. No C
call frame is unwound: the trap does not return into, and is not observable by,
the C code that is on the stack. Rue has no unwinding mechanism, so this
abort-at-boundary behavior is the only defined outcome; the `"C-unwind"` ABI
string is reserved for a future richer policy and is rejected in this version.

## Reverse-direction undefined behavior

{{ rule(id="9.3:3", cat="undefined-behavior") }}

It is undefined behavior for a foreign transfer of control that crosses a Rue
call frame — a C++ or Objective-C exception, or a `longjmp` whose matching
`setjmp` is on the far side of one or more Rue frames — to propagate through Rue
code. Rue defines no mechanism to intercept, unwind, or resume such a transfer,
and a compiled Rue frame preserves no state that would make one well-defined.
This is the mirror of the abort-at-boundary rule (§ 9.3:2): just as a Rue trap
never unwinds a C frame, a foreign non-local exit must never unwind a Rue frame.

## Ownership at the boundary

{{ rule(id="9.3:4", cat="normative") }}

Passing a value to a foreign function is a move: its storage is handed to the
foreign side and the Rue destructor does not run for it. A linear or
destructor-bearing value may not cross the boundary *by value* — it is not
FFI-safe (ADR-0064 Amendment 1) and is rejected — so ownership of such a value
crosses only as a `@raw` / `@raw_mut` raw pointer that escapes into C under the
programmer-responsibility rules of § 9 (ADR-0028). Across the boundary the
compiler enforces no lifetime, exclusivity, aliasing, or destructor guarantee on
a pointer handed to or received from C; honoring the foreign side's ownership and
lifetime contract is the programmer's responsibility, exactly as for the raw
pointer and heap intrinsics of § 9.2.

## Foreign redeclaration

{{ rule(id="9.3:5", cat="normative") }}

A foreign declaration describes an external C symbol rather than defining a Rue
function, so the same symbol **MAY** be declared in more than one module of a
program; all such declarations name one symbol, and one definition is linked in.
Every declaration of one symbol **MUST** declare the same signature: the same
number of parameters, pairwise-equal parameter types and passing modes, and the
same return type. Parameter names are not part of the signature, and types are
compared as resolved in each declaring module, so a nominal type declared
separately in two modules is two distinct types even under one name. A program
in which two foreign declarations of one symbol disagree is ill-formed and is
rejected at compile time, naming both declaration sites; a redeclaration that
agrees is accepted, as in C.

## The program entry point is not a foreign function

{{ rule(id="9.3:6", cat="legality-rule") }}

A foreign declaration **MUST NOT** name `main`, and a `pub extern "C" fn` export
**MUST NOT** be named `main`. The `main` symbol is the program's own entry point,
reserved for the runtime start glue that invokes the root module's `main`
(§ 6.1:38); no external definition of it can be linked in. Because a foreign
declaration names the C symbol it declares, `extern "C" { fn main(...); }` would
otherwise bind the program's own entry point — in the root module colliding with
its definition, and in any other module resolving a call through the declaration
back into `main` itself. The rule applies in every module and for every declared
signature, including one that agrees with the entry point's; a program containing
such a declaration or export is ill-formed and is rejected at compile time.
