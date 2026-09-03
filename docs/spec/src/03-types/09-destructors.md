+++
title = "Destructors"
weight = 9
+++

# Destructors

This section describes when and how values are dropped in Rue.

## Drop Semantics

{{ rule(id="3.9:1", cat="normative") }}

When a value's owning binding goes out of scope and the value has not been moved out of it, the value is *dropped*. Dropping runs the value's *drop glue* — its user destructor if it has one, then the drop of its droppable contents (3.9:28) — as fixed by the drop relation in `docs/formal/01-core-calculus.md` §6.11.

{{ rule(id="3.9:2", cat="normative") }}

A value is dropped exactly once. A move sets the source place to moved-out, and the drop relation skips a moved-out place (`docs/formal/01-core-calculus.md` §6.11): so a moved value is not dropped at its original binding site but through its final owner. This is the no-double-free guarantee (`docs/formal/01-core-calculus.md` §7).

{{ rule(id="3.9:3", cat="example") }}

```rue
struct Data { value: i32 }

fn consume(d: Data) -> i32 { d.value }

fn main() -> i32 {
    let d = Data { value: 42 };
    consume(d)  // d is moved, dropped inside consume()
}  // d is NOT dropped here (was moved)
```

## Drop Order

{{ rule(id="3.9:4", cat="normative") }}

When multiple values go out of scope at the same point, they are dropped in reverse declaration order (last declared, first dropped).

{{ rule(id="3.9:5", cat="example") }}

```rue
fn main() -> i32 {
    let a = Data { value: 1 };  // declared first
    let b = Data { value: 2 };  // declared second
    0
}  // b dropped first, then a
```

{{ rule(id="3.9:6", cat="informative") }}

Reverse declaration order (LIFO) ensures that values declared later, which may depend on earlier values, are cleaned up first.

## Trivially Droppable Types

{{ rule(id="3.9:7", cat="normative") }}

A type is *trivially droppable* if dropping it requires no action. Trivially droppable types have no destructor.

{{ rule(id="3.9:8", cat="normative") }}

The following types are trivially droppable:
- All integer types (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`)
- The boolean type (`bool`)
- The unit type (`()`)
- The never type (`!`)
- Discriminant-only enum types (every variant is payload-free)
- Arrays of trivially droppable types

A payload-carrying enum is **not** trivially droppable unless every payload is
itself trivially droppable. Dropping such an enum runs the drop glue of its
**active** variant's payload exactly once, and nothing for a discriminant-only
active variant (6.3:20).

{{ rule(id="3.9:9", cat="normative") }}

A struct type is trivially droppable if all of its fields are trivially droppable.

{{ rule(id="3.9:10", cat="example") }}

```rue
// Trivially droppable: all fields are trivially droppable
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    p.x  // p is trivially dropped (no-op)
}
```

## Types with Destructors

{{ rule(id="3.9:11", cat="normative") }}

A type has a destructor if dropping it requires cleanup actions. When such a type is dropped, its destructor is invoked.

{{ rule(id="3.9:12", cat="normative") }}

A struct has a destructor if any of its fields has a destructor, or if the struct has a user-defined destructor.

{{ rule(id="3.9:13", cat="normative") }}

For a struct with a destructor, fields are dropped in declaration order (first declared, first dropped).

{{ rule(id="3.9:14", cat="normative") }}

An array type `[T; N]` has a destructor if its element type `T` has a destructor.

{{ rule(id="3.9:15", cat="dynamic-semantics") }}

When an array with a destructor is dropped, each element is dropped in index order (element 0 first, then element 1, and so on).

{{ rule(id="3.9:16", cat="example") }}

```rue
fn main() -> i32 {
    let arr: [StrBuf; 3] = ["a", "b", "c"];
    0
}  // Each StrBuf in arr is dropped: arr[0], arr[1], arr[2]
```

{{ rule(id="3.9:17", cat="informative") }}

The distinction between "drop order of bindings" (reverse declaration) and "drop order of fields/elements" (declaration/index order) matches C++ and Rust behavior. Bindings use LIFO for dependency correctness; fields and array elements use forward order for consistency with construction order.

## Drop Placement

{{ rule(id="3.9:18", cat="dynamic-semantics") }}

Drops are inserted at the following points:
- At the end of a block scope, for all live bindings declared in that scope
- Before a `return` statement, for all live bindings in all enclosing scopes
- Before a `break` statement, for all live bindings declared inside the loop
- At an assignment, for the destination's previous value: when an assignment overwrites a place (a variable, struct field, array element, or `inout` parameter) whose current value is live (not moved out), the overwritten value is dropped after the right-hand side has been fully evaluated and before the new value is stored. This implicit drop applies only to places whose type does **not** carry a linear value (3.8:57); overwriting a live *linear* place is instead a compile-time error (3.8:77), because a linear value may never be dropped implicitly.
- At the end of an expression statement whose result is discarded (including a `let _ = ...` statement), for the discarded result value

{{ rule(id="3.9:19", cat="dynamic-semantics") }}

Each branch of a conditional independently drops bindings declared within that branch.

{{ rule(id="3.9:20", cat="example") }}

```rue
fn example(condition: bool) -> i32 {
    let a = Data { value: 1 };
    if condition {
        let b = Data { value: 2 };
        return 42;  // b dropped, then a dropped, then return
    }
    let c = Data { value: 3 };
    0  // c dropped, then a dropped
}
```

## Code Generation

{{ rule(id="3.9:21", cat="dynamic-semantics") }}

When a non-trivially droppable value is dropped, the compiler generates a call to the value's destructor function.

{{ rule(id="3.9:22", cat="dynamic-semantics") }}

When a trivially droppable value is dropped, no code is generated. The drop is elided as a no-op.

{{ rule(id="3.9:23", cat="informative") }}

The distinction between trivially droppable and non-trivially droppable types allows the compiler to avoid generating unnecessary cleanup code for simple types like integers and structs containing only integers.

## User-Defined Destructors

{{ rule(id="3.9:24", cat="syntax") }}

A user-defined destructor is declared using the `drop fn` syntax:

```rue
drop fn TypeName(self) {
    // cleanup code
}
```

{{ rule(id="3.9:25", cat="normative") }}

A user-defined destructor for a *named* struct type **MUST** be declared as a top-level declaration. Methods and associated functions are declared inline in struct bodies (6.4), not as separate top-level declarations. The destructor **MUST** take exactly one parameter named `self` and return nothing (implicit unit type). (A destructor for an *anonymous* struct type is declared inside the struct body instead; see 3.9:41.)

{{ rule(id="3.9:26", cat="legality-rule") }}

Each struct type **MAY** have at most one user-defined destructor. A compile-time error is raised if multiple destructors are declared for the same type.

{{ rule(id="3.9:27", cat="legality-rule") }}

A user-defined destructor can only be declared for a struct type that is defined
in the same module. Destructor target lookup is module-local: a struct defined
in another loaded module does not satisfy the declaration. A compile-time error
is raised if the destructor references an unknown type or a non-struct type.

{{ rule(id="3.9:28", cat="dynamic-semantics") }}

When a value with a user-defined destructor is dropped, the user-defined destructor runs first, followed by the automatic dropping of any fields that have destructors.

{{ rule(id="3.9:29", cat="example") }}

```rue
struct FileHandle {
    fd: i32,
}

drop fn FileHandle(self) {
    // Close the file descriptor
    close(self.fd);
}
```

{{ rule(id="3.9:30", cat="informative") }}

The `drop fn` syntax was chosen because it clearly indicates the purpose of the function while being distinct from regular functions and methods. The destructor is a top-level declaration rather than an inline struct-body method or associated function because it has special calling semantics: it is invoked automatically by the compiler when values go out of scope.

{{ rule(id="3.9:31", cat="legality-rule") }}

A type declared `@copy` **MUST NOT** have a user-defined destructor. A compile-time error is raised if a `drop fn` is declared for a `@copy` type.

{{ rule(id="3.9:32", cat="informative") }}

Copies of a `@copy` value are implicit and untracked, so each copy would run the destructor again — cleaning up the same logical resource multiple times. This mirrors Rust, where a type cannot implement both `Copy` and `Drop`.

{{ rule(id="3.9:33", cat="legality-rule") }}

Within a user-defined destructor, `self` **MUST NOT** be moved out (to a call argument, a new binding, a by-value method receiver, or any other new owner). A compile-time error is raised if the destructor body moves `self`. The new owner would drop the value again at its own scope exit, re-entering the destructor.

{{ rule(id="3.9:34", cat="legality-rule") }}

A field **MUST NOT** be moved out of a value whose type has a user-defined destructor. This applies to every enclosing value along a field path (moving `t.a.b` moves out of both `t` and `t.a`), and includes `self` within the type's own destructor. A compile-time error is raised for such a move. Borrowing such a field (`borrow` or `inout`) is permitted, as is moving the whole value.

{{ rule(id="3.9:35", cat="informative") }}

The destructor always runs on the whole value when it is dropped: it would observe the moved-out field, and the automatic field cleanup that follows the destructor would drop the moved field a second time. Moving a field out of a struct *without* a user-defined destructor remains legal even when the field's own type has one — the field's drop at the struct's scope exit is simply suppressed.

{{ rule(id="3.9:36", cat="example") }}

```rue
struct Inner { v: i32 }

drop fn Inner(self) { @dbg(self.v); }

struct Outer { f: Inner }

drop fn Outer(self) { @dbg(self.f.v); }

fn eat(i: Inner) -> i32 { i.v }

fn main() -> i32 {
    let o = Outer { f: Inner { v: 7 } };
    eat(o.f)  // ERROR: cannot move field `f` out of a value of type 'Outer'
}
```

## The `@drop` intrinsic

{{ rule(id="3.9:37", cat="normative") }}

The `@drop(x)` intrinsic runs the drop glue of its operand `x` — the operand's destructor, if any, followed by the recursive drop of its still-owned fields and array elements — at the point of the call, and consumes `x`. If `x` is partially moved, moved-out sub-places are skipped and only the owned residue is destroyed. It is the deliberate, visible discard of a value: "the drop that would otherwise run at scope exit, invoked by hand." `@drop` is memory-safe and requires no `checked` context.

{{ rule(id="3.9:38", cat="dynamic-semantics") }}

`@drop(x)` consumes `x`: after it, `x` is moved-from, so using `x` again is a use-after-move error (E0205), and the scope-exit drop that would otherwise run for `x` is suppressed. For a partially moved `x`, this applies to the residue as a whole: each still-owned sub-place is dropped once at the `@drop` site, while each moved-out sub-place remains suppressed. Together these preserve the "dropped exactly once" invariant.

{{ rule(id="3.9:39", cat="dynamic-semantics") }}

Applied to a `linear` value, `@drop(x)` both runs the glue and satisfies the must-consume obligation (the value is no longer reported as an unconsumed linear value, E0406). Applied to an affine value, it runs the glue early — deterministic cleanup before the end of the enclosing scope. Applied to a `@copy` value, it is a no-op, because a `@copy` value carries no drop glue and no consumption obligation.

{{ rule(id="3.9:40", cat="example") }}

```rue
linear struct Guard { v: i32 }

drop fn Guard(self) { @dbg(self.v); }

fn main() -> i32 {
    let g = Guard { v: 42 };
    @dbg(1);
    @drop(g);  // runs Guard's destructor here: prints 42
    @dbg(2);
    0          // g is already consumed — no drop at scope exit
}
// prints: 1, 42, 2
```

## Destructors on anonymous (generic) struct types

{{ rule(id="3.9:41", cat="syntax") }}

A struct type produced by a comptime `-> type` function is *anonymous* — it has
no name to key a top-level `drop fn Name(self)` on. Its destructor is instead
declared **inside the struct body**, using a name-less `drop fn`:

```rue
fn Buf(comptime T: type) -> type {
    struct {
        buf: ptr mut T,
        cap: u64,
        drop fn(self) {
            checked {
                let block: ptr mut u8 = @int_to_ptr(@ptr_to_int(self.buf));
                @free(block, self.cap * @intCast(@size_of(T)), @intCast(@align_of(T)));
            };
        }
    }
}
```

The in-body destructor **MUST** take exactly one by-value `self` parameter and
return nothing (implicit unit type). A receiver mode keyword (`borrow` /
`inout`) is not permitted on a destructor's `self`.

{{ rule(id="3.9:42", cat="normative") }}

An anonymous struct's in-body destructor has the same semantics as a named
struct's top-level `drop fn`: it is monomorphized together with the struct at
each instantiation and runs on every value of the resulting concrete type when
that value is dropped (at scope exit or via `@drop`), before the automatic
dropping of the value's fields. All destructor legality rules (3.9:26 — at most
one per type; 3.9:31 — a `@copy` type cannot have one, so a struct with an
in-body `drop fn` is never `@copy`; 3.9:33 — `self` may not be moved out;
3.9:34 — a field may not be moved out) apply unchanged.

{{ rule(id="3.9:43", cat="example") }}

```rue
fn Tag(comptime T: type) -> type {
    struct {
        v: T,
        fn make(x: T) -> Self { Self { v: x } }
        drop fn(self) { @dbg(self.v); }
    }
}

fn main() -> i32 {
    let G = Tag(i32);
    let a = G.make(1);
    let b = G.make(2);
    @dbg(100);
    0
}
// prints: 100, 2, 1  (values drop in reverse declaration order at scope exit)
```
