---
id: 0096
title: "Second-class function parameters: callbacks that are passed, called, and forwarded, never stored"
status: implemented
tags: [language, syntax, semantics, types, abi, ownership]
feature-flag: fn_params
created: 2026-09-13
accepted: 2026-09-13
implemented: 2026-09-14
spec-sections: ["6.1:46", "6.1:47", "6.1:48", "6.1:49", "6.1:50", "6.1:51", "6.1:52"]
superseded-by:
relates: ["RUE-2107", "RUE-2112", "RUE-2113", "RUE-2193", "RUE-2194", "RUE-2195", "RUE-1886", "ADR-0005", "ADR-0043", "ADR-0084", "ADR-0087"]
---

# ADR-0096: Second-class function parameters: callbacks that are passed, called, and forwarded, never stored

## Status

Implemented. Accepted on 2026-09-13 by Steve (the RUE-2107 ruling: the
researched v1 is ratified as written). Phases 1 through 3 delivered the `fn`
parameter type, the callback semantics through CFG and the reference
interpreter, and the native indirect call on both backends behind the
`fn_params` preview feature; phase 4 (RUE-2113) validated the feature against
the RUE-2107 exit criteria and removed the gate on 2026-09-14, so the subset
decided below is stable Rue. The measurements phase 4 rests on are in
[docs/notes/fn-params-indirect-call-overhead.md](../notes/fn-params-indirect-call-overhead.md).

### Deferred

Everything outside the decided subset stays undecided, not rejected. Each item
below is a later design with its own ADR, and the escape rule (decision 5) was
chosen so that none of them is constrained by this one:

- **Closures.** A callback has no environment: nothing is captured, and the
  context a comparator or predicate needs is an explicit `borrow` or `inout`
  argument the caller passes at every call.
- **Storage.** A `fn` type is legal only as a by-value runtime parameter
  (E0214) and a callback parameter may only be called or forwarded (E0217).
  Keeping a callback in a local, a field, an array, a container, an `Option`,
  or a return value, and comparing or converting it, wait for a first-class
  function-value design.
- **Extern callbacks.** An `extern "C"` function is not an eligible argument
  (E0216) and a Rue callback never crosses the C boundary in either direction;
  a named Rue wrapper around a `checked` foreign call is the one spelling.
  Passing a Rue function to C, and receiving a C function pointer, need the
  ADR-0064 boundary to say what a code pointer means on each side.
- **Generic and receiver adaptation.** A generic function and a method with a
  receiver are not eligible (E0216); a monomorphic wrapper names the
  specialization or takes the receiver explicitly. The spelling for naming a
  generic specialization directly, and unbound `Type.method` conversion,
  remain open questions below.

## Summary

A function may take a callback:

```rue
struct Policy { descending: bool }

fn precedes(borrow p: Policy, a: i64, b: i64) -> bool {
    if p.descending { a > b } else { a < b }
}

fn choose(borrow p: Policy, a: i64, b: i64,
          cmp: fn(borrow Policy, i64, i64) -> bool) -> i64 {
    if cmp(borrow p, a, b) { a } else { b }
}

fn forward(borrow p: Policy, a: i64, b: i64,
           cmp: fn(borrow Policy, i64, i64) -> bool) -> i64 {
    choose(borrow p, a, b, cmp)
}
```

The parameter type `fn(borrow Policy, i64, i64) -> bool` names a signature:
parameter modes and types in order and a result. A caller passes a named
function whose signature is exactly that one; the body calls the parameter
with ordinary call rules or forwards it to another parameter of the same
type. That is the whole feature. A callback is second-class: it has no
first-class value, so it cannot be stored in a local, a field, or an array,
returned, compared, converted to a pointer, or captured. Behavior that must
persist is passed again at each call.

## Context

The prototype ports measured for RUE-2107 need comparators and predicates
over identifiers that carry external context: a
sort keyed by a policy, a visitor over a table, a filter over a buffer. Rue
has no way to pass behavior at all today, so each such algorithm is
duplicated per comparison or written against a fixed one.

The smallest design that serves those ports is a callback parameter with no
environment. It reuses everything the language already has: the declaration's
access modes (`borrow`, `inout`) spell how the callback receives each
argument, the native calling convention (ADR-0084) places the arguments, and
the exclusivity rules (6.1:20, 6.1:30, 6.1:36) apply through the callback's
signature exactly as at a direct call. Context reaches the callback the same
way it reaches any function: as an explicit `borrow` or `inout` argument.

Closures, first-class function values, and callbacks that cross the C
boundary are each a larger design with their own storage, capture, and ABI
questions. This ADR does not decide them; its escape rule is deliberately
stronger than a lifetime rule so that the first implementation is small and
the later designs are not constrained by it.

## Decision

1. **Syntax.** `fn_type = "fn" "(" [ fn_type_params ] ")" [ "->" type ]`,
   with `fn_type_param = [ "inout" | "borrow" ] type`. Modes precede types,
   no parameter is named, and an omitted result is `()`. A `fn` type may
   nest inside another `fn` type's parameter list. `comptime` is not a
   callback mode.
2. **Position.** A `fn` type is legal only as the type of a by-value runtime
   parameter of a function, method, or associated function, including a
   parameter of another `fn` type. It is rejected (E0214) as a return type,
   a `let` or `const` annotation, a struct field, an enum payload, an array
   element, a pointer pointee, a slice element, a type argument, and the type
   of a `borrow`, `inout`, or `comptime` parameter. The callback parameter
   itself is immutable and carries no mode.
3. **Identity.** Two `fn` types are the same type when they have the same
   arity, the same mode and type at each position, and the same result type
   after ordinary type resolution. Parameter names never take part. There is
   no conversion between `fn` types: no integer widening, no mode
   adaptation, no variance.
4. **Eligible arguments.** The argument to a `fn` parameter is a named
   function whose signature is exactly the parameter's: an ordinary
   monomorphic free function, a module-qualified function, a compile-time
   alias to one, or a receiverless associated function of a concrete type,
   subject to ordinary visibility. Naming the function makes its body
   reachable. A method with a receiver, a generic function, an `extern "C"`
   or `unchecked` function, an accessor, and a builtin are not eligible; a
   named wrapper is the one spelling for each of those.
5. **Uses.** Inside the body a callback parameter has three uses: it is
   called (`cb(a, borrow b)`, ordinary call rules, invoked zero or more
   times), it is forwarded as the argument to another `fn` parameter of the
   same type (any number of times), and nothing else. Every other read of the
   parameter is an escape and is rejected.
6. **Representation.** One non-null code pointer with no environment: one
   general-purpose register slot, trivially copied, never dropped. Arguments
   and results cross the indirect call through the native convention
   (ADR-0084) exactly as through a direct call, hidden results and
   by-reference arguments included. Devirtualizing a known target is an
   optimization, never a semantic.
7. **Labels.** Calls through a `fn` parameter are positional. If RUE-1886
   later adds argument labels to named calls, calls through `fn` stay
   positional and this ADR is the documented exception.

## Implementation Phases

- [x] **Phase 1: the `fn` parameter type** - RUE-2193. Parser, RIR, AIR
  type pool, semantic resolution behind `fn_params`, the position rule, and
  the durable identity and import encodings, so a declaration may name a
  callback parameter.
- [x] **Phase 2: callback semantics through CFG** - RUE-2194. Binding a
  named function to a `fn` parameter with exact signature matching (E0215),
  the eligible argument forms (E0216), calling through the parameter,
  forwarding, every escape rejection (E0217), the `fn_ref`/`call_indirect`
  AIR and `fn_addr`/`call_indirect` CFG instructions with their durable
  encodings, and the oracle interpreter. Native code generation refuses a
  function that binds or calls a callback until phase 3.
- [x] **Phase 3: native indirect calls** - RUE-2195. Function-address
  materialization (`lea rip`; `adrp`+`add`, relocated like a string
  constant's address) and the register-indirect call (`call r64`; `blr`)
  through the canonical call plan on x86-64 and AArch64, placed after the
  argument leaves in a register the argument sequence never writes, with
  executable spec, CLI and oracle-diff coverage.
- [x] **Phase 4: validation and stabilization** - RUE-2113. `std.sort`
  gained `sort_by`, `insertion_sort_by`, `is_sorted_by` and `partition_by`, a
  comparator and a predicate API that take an explicit context, and the
  Mosaic example ranks its search terms through `sort_by` with the index as
  the comparator's context; the CLI cases cover float, two-slot and
  hidden-result callbacks, an `inout` aggregate context, and a callback
  forwarded across three modules; the overhead of the indirect call was
  measured; and the `fn_params` preview gate was removed for exactly the
  subset above.

## Consequences

### Positive

- Comparators, predicates, and visitors take their context explicitly, so
  the existing exclusivity rules cover them with no new aliasing story.
- No allocation, no environment pointer, no new calling convention.
- The escape rule keeps the type out of every storage position, so the
  later closure or function-value design starts from a clean slate.

### Negative

- A callback cannot be kept: an event registry, a deferred call, or a
  stored comparator needs the later design or a restructuring that passes
  behavior at each operation.
- Generic functions are not inferred from the expected callback type; a
  monomorphic wrapper with explicit `comptime` arguments names the
  specialization.

## Open Questions

- Whether unbound instance-method conversion (`Type.method` as a callback
  taking the receiver first) should be admitted later. v1 excludes it.
- The spelling for naming a generic specialization as a callback argument.

## Future Work

Closures with capture, first-class function values in storage, callbacks
across the C boundary, and argument labels on calls through `fn` (RUE-1886).

## References

- [Specification 6.1](../spec/src/06-items/01-functions.md)
- [ADR-0043: The collection and string type trio, whose slices are the second-class precedent](0043-collection-string-type-trio.md)
- [ADR-0084: The native Rue calling convention](0084-native-calling-convention.md)
- [ADR-0087: No function overloading](0087-no-function-overloading-one-name-one-signature.md)
- [ADR-0005: Preview Features](0005-preview-features.md)
