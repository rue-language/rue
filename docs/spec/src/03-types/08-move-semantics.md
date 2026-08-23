+++
title = "Move Semantics"
weight = 8
+++

# Move Semantics

This section describes how values are moved and copied in Rue.

## Value Categories

{{ rule(id="3.8:1", cat="normative") }}

Types in Rue are categorized by how they behave when *used* (3.8:76):
- **Copy types** are implicitly duplicated by a use; using a Copy value does not consume the original.
- **Move types** (also called affine types) are consumed by a use; after a move type value is used, the original binding becomes invalid.

{{ rule(id="3.8:2", cat="normative") }}

The following types are Copy types:
- All integer types (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`)
- The boolean type (`bool`)
- The unit type (`()`)
- The first-class string types: `str` (3.7:44) and the fixed inline buffers
  `Str(N)` (3.7:50)
- Discriminant-only enum types (every variant is payload-free, C-like)
- Array types `[T; N]` where `T` is a Copy type

A payload-carrying enum is **not** unconditionally Copy: its multiplicity is the
join of its variants' payload multiplicities over Copy ⊑ Affine ⊑ Linear
(6.3:19). Such an enum is Copy only when every payload is itself Copy; a variant
carrying a move (affine or linear) payload makes the whole enum a move type.

{{ rule(id="3.8:3", cat="normative") }}

User-defined struct types are move types by default. Using a struct value consumes it.

{{ rule(id="3.8:4", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is moved to q
    // p is no longer valid here
    q.x + q.y
}
```

## Using a Value

{{ rule(id="3.8:76", cat="informative") }}

The single operation underlying moves, copies, and linear consumption is the *use* of a value. Every occurrence of a place expression — a binding, a field projection, or an array index — sits in exactly one of two syntactic contexts. In a **place context** the occurrence denotes a location and the value stored there is not consumed by appearing there; the place contexts are exactly the target of an assignment, the base of a field projection or array index, the operand of a `borrow` or `inout` argument, and the operand of an equality comparison (`==`/`!=`), which is read through a shared borrow rather than consumed (4.3:3f). Every other occurrence is a **value context** — an arithmetic, bitwise, or ordering operator operand, the scrutinee of an `if` or `match`, a struct-field or array-element initializer, a by-value function argument, the operand of `return`, or the tail expression of a block — and such an occurrence is a **use** of the place. The effect of a use depends only on the value category of the place's type: a use of a Copy type copies the value and leaves the place valid (3.8:9); a use of a move type, whether affine or linear, moves — equivalently, *consumes* — the value, leaving the place invalid until it is reinitialized (3.8:7, 3.8:33). A use of a field projection or a constant array index moves only that sub-place, a partial move (3.8:22). The enumerations below — the move contexts (3.8:7), the linear consumption contexts (3.8:33), and the repeated use of Copy values (3.8:9) — are each a consequence of this one definition: which occurrences are value contexts, and which value category the used type has. The core calculus, `docs/formal/01-core-calculus.md` §4.2, states this precisely (place context versus value context, and the copy-versus-move effect of a use); the present paragraph is its informal gloss, and the formal definition governs.

## The `@copy` Directive

{{ rule(id="3.8:14", cat="normative") }}

A struct type **MAY** be declared as a Copy type using the `@copy` directive before the struct definition.

{{ rule(id="3.8:15", cat="syntax") }}

```ebnf
copy_struct = "@copy" struct_def ;
```

{{ rule(id="3.8:16", cat="normative") }}

A struct marked with `@copy` is a Copy type. Using a `@copy` struct value does not consume it; the value is implicitly duplicated.

{{ rule(id="3.8:17", cat="example") }}

```rue
@copy
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is copied, not moved
    let r = p;      // p can be used again
    p.x + q.x + r.x // all three are valid
}
```

{{ rule(id="3.8:18", cat="legality-rule") }}

A `@copy` struct **MUST** contain only fields that are themselves Copy types. It is a compile-time error to mark a struct as `@copy` if any of its fields are move types.

{{ rule(id="3.8:19", cat="example") }}

```rue
struct Inner { value: i32 }  // move type (no @copy)

@copy
struct Outer { inner: Inner }  // ERROR: field 'inner' has non-Copy type 'Inner'
```

{{ rule(id="3.8:20", cat="normative") }}

A `@copy` struct **MAY** contain fields of primitive Copy types (integers, booleans, unit), first-class string types (`str`, `Str(N)` — 3.7:44, 3.7:50), discriminant-only enum types, arrays of Copy types, or other `@copy` struct types. A field whose type is a payload-carrying enum is admissible only when that enum is itself Copy under the join of 6.3:19 — that is, only when every one of its payloads is Copy; a `@copy` struct field must itself be a Copy type (3.8:18), so a move-typed enum field is rejected.

{{ rule(id="3.8:21", cat="example") }}

```rue
@copy
struct Point { x: i32, y: i32 }

@copy
struct Rect { top_left: Point, bottom_right: Point }  // OK: Point is @copy

fn main() -> i32 {
    let r = Rect {
        top_left: Point { x: 0, y: 0 },
        bottom_right: Point { x: 10, y: 10 }
    };
    let r2 = r;     // r is copied
    r.top_left.x    // r is still valid
}
```

## Linear Types

{{ rule(id="3.8:30", cat="normative") }}

A struct type **MAY** be declared as a linear type using the `linear` keyword before the struct definition.

{{ rule(id="3.8:31", cat="syntax") }}

```ebnf
linear_struct = "linear" "struct" IDENT "{" [ struct_fields ] "}" ;
```

{{ rule(id="3.8:32", cat="normative") }}

A linear type **MUST** be explicitly consumed. It is a compile-time error for a linear value to go out of scope without being consumed by a function call.

{{ rule(id="3.8:33", cat="normative") }}

Consuming a linear value is the same operation as moving it: any use of a linear value (3.8:76) consumes it. Passing it as a by-value argument (the function becomes the consumer) and returning it (the caller becomes responsible for consuming it) are both uses, and therefore each consumes the value. Moving a field out of a value whose struct type is declared `linear` *destructures* it: because the obligation belongs to the value itself and not to its contents (3.8:74), the field access consumes the whole value, and the other fields are dropped with it (subject to 3.8:60). The value so consumed is the *place* that is destructured, which is the binding as a whole only when the declared-`linear` struct is the binding's own type: `h.arr[0].v` destructures the element `h.arr[0]`, and `h.a.v` the field `h.a`. Every sub-place of an enclosing value that is not part of the destructured value — a sibling element, a sibling field, the rest of the binding — is untouched by the destructure: it keeps its own must-consume obligation (3.8:32) and is dropped by the ordinary scope-exit walk (3.9:2). When the access chain passes through several declared-`linear` levels, the outermost one is destructured, and reaching a deeper level consumes that outer value with its residue. A field access on a struct that is linear only by infection (3.8:58) is not a destructure; it is the ordinary partial move of 3.8:22, and the carrier's obligation is discharged sub-place by sub-place (3.8:60).

{{ rule(id="3.8:34", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn consume(m: MustUse) -> i32 { m.value }

fn main() -> i32 {
    let m = MustUse { value: 42 };
    consume(m)  // OK: m is consumed
}
```

{{ rule(id="3.8:35", cat="legality-rule") }}

It is a compile-time error to allow a linear value to be implicitly dropped.

{{ rule(id="3.8:36", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn main() -> i32 {
    let m = MustUse { value: 1 };  // ERROR: linear value dropped without being consumed
    0
}
```

{{ rule(id="3.8:37", cat="legality-rule") }}

A linear struct **MUST NOT** be marked with `@copy`. Linear types cannot be implicitly copied.

{{ rule(id="3.8:38", cat="example") }}

```rue
@copy
linear struct Invalid { value: i32 }  // ERROR: linear types cannot be @copy
```

{{ rule(id="3.8:50", cat="legality-rule") }}

A linear value **MUST** be consumed on every control-flow path on which it goes out of scope. It is a compile-time error for a linear value to be consumed in only some branches of a conditional (`if`/`else` or `match`): the paths that do not consume it would drop it implicitly.

{{ rule(id="3.8:51", cat="legality-rule") }}

A control-flow path that diverges before a scope's end never reaches that scope's end, and so is exempt from the consumption check applied *there* on that path (the diverging path contributes nothing to 3.8:50's branch requirement). Divergence is not an amnesty from consumption: an exit that unwinds scopes — a `return` expression (including the implicit return of a `?` expression's failure path, 4.15:7), a `break`, or a `continue` — ends the scopes it exits at the exit itself and drops their live bindings there (4.9:7, 4.8:21), so a linear value held by any scope the exit unwinds — including, for a `return` or `?`, a pass-by-value parameter (3.8:62) — **MUST** already have been consumed in the state in force when the exit executes. It is a compile-time error otherwise, exactly as at the scope's end (3.8:32, 3.8:50): the exit's drops would otherwise destroy the value unconsumed.

{{ rule(id="3.8:52", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn consume(m: MustUse) -> i32 { m.value }

fn main() -> i32 {
    let m = MustUse { value: 1 };
    if true {
        consume(m)   // ERROR: 'm' is not consumed on the else path
    } else {
        0
    }
}
```

{{ rule(id="3.8:57", cat="normative") }}

A type *carries a linear value* if it is a linear struct type, an array type whose element type carries a linear value, a struct type with a field whose type carries a linear value, or an enum type any of whose variants has a payload component whose type carries a linear value (the payload join of 6.3:19 — the active variant is not known statically, so the type's worst case governs). Pointer types do not carry a linear value (a pointer does not own its pointee).

{{ rule(id="3.8:58", cat="normative") }}

Linearity is infectious: a struct type that is not declared `linear` but has a field whose type carries a linear value is itself a linear type. If the containing struct could be implicitly dropped, the linear field would be silently dropped with it.

{{ rule(id="3.8:59", cat="example") }}

```rue
linear struct MustUse { value: i32 }

struct Wrap { m: MustUse }   // not declared linear, but linear by 3.8:58

fn main() -> i32 {
    let w = Wrap { m: MustUse { value: 1 } };  // ERROR: linear value 'w' dropped
    0
}
```

{{ rule(id="3.8:60", cat="legality-rule") }}

It is a compile-time error for a field access that *destructures* a value whose struct type is declared `linear` (3.8:33) to implicitly drop a *different* field that carries a linear value. Every declared-`linear` struct level along the access path is checked: destructuring consumption extracts the accessed field and drops the rest of the value, so a linear sibling in that residue would be silently dropped.

A field access on a struct that is linear only by infection (3.8:58) does **not** destructure it. It is an ordinary partial move of exactly the accessed field (3.8:22): sibling fields remain accessible (3.8:22 for move-typed siblings, 3.8:28 for Copy ones), and the non-moved residue is dropped by the ordinary scope-exit walk of the binding (3.9:2). Such a carrier's must-consume obligation (3.8:32) attaches to its linear *sub-places* rather than to the carrier as a whole: it is discharged by consuming each linear sub-place on every path, and a linear sub-place left unconsumed is reported where it is dropped — at scope exit — not at a sibling's access. Consuming a linear sub-place therefore leaves the siblings, including any destructor they carry, to drop normally.

{{ rule(id="3.8:61", cat="example") }}

```rue
linear struct MustUse { value: i32 }

struct Container { inner: MustUse, tag: i32 }

fn sink(m: MustUse) -> i32 { m.value }

fn main() -> i32 {
    let c = Container { inner: MustUse { value: 1 }, tag: 2 };
    c.tag            // ERROR: 'c.inner' is never consumed (reported at scope exit)
}

fn ok() -> i32 {
    let c = Container { inner: MustUse { value: 1 }, tag: 2 };
    sink(c.inner)    // OK: consumes the linear field; 'tag' (non-linear) is dropped
}

fn also_ok() -> i32 {
    let c = Container { inner: MustUse { value: 1 }, tag: 2 };
    let t = c.tag;   // OK: 'tag' is Copy, so reading it moves nothing (3.8:28)
    sink(c.inner) + t
}
```

The declared-`linear` case is the one this rule rejects at the access itself, because there the access really does drop the siblings:

```rue
linear struct MustUse { value: i32 }

linear struct Pair { inner: MustUse, tag: i32 }

fn main() -> i32 {
    let p = Pair { inner: MustUse { value: 1 }, tag: 2 };
    p.tag            // ERROR: destructuring 'p' would implicitly drop linear field 'inner'
}
```

{{ rule(id="3.8:62", cat="legality-rule") }}

A function owns its pass-by-value parameters and drops them when it returns unless they are moved out. Therefore a pass-by-value parameter whose type carries a linear value **MUST** be consumed by the function body on every non-diverging control-flow path, exactly as for a linear local binding (3.8:32, 3.8:50). `borrow` and `inout` parameters are exempt: the caller retains ownership. A destructor's `self` parameter is also exempt: it is disposed of by the drop glue after the destructor body runs (see 3.9), and moving it out is rejected.

{{ rule(id="3.8:63", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn bad(m: MustUse) -> i32 { 0 }          // ERROR: 'm' is dropped, not consumed

fn good(m: MustUse) -> i32 { m.value }   // OK: destructuring consumes 'm'
```

{{ rule(id="3.8:64", cat="legality-rule") }}

It is a compile-time error to discard an expression value whose type carries a linear value. A value is discarded when it is the value of a non-final expression statement in a block, or the result value of a loop body (which is discarded on every iteration).

{{ rule(id="3.8:65", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn make_linear() -> MustUse { MustUse { value: 1 } }

fn main() -> i32 {
    make_linear();   // ERROR: discarded linear value
    0
}
```

{{ rule(id="3.8:66", cat="legality-rule") }}

The consumption requirement (3.8:32) applies to every binding whose type carries a linear value (3.8:57), not only to bindings of linear struct type. In particular, an array whose element type carries a linear value **MUST** be consumed — either as a whole (for example, by passing the array to a function by value) or element-wise via constant-index moves (3.8:71); dropping the array would silently drop every element.

{{ rule(id="3.8:67", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn make_linear() -> MustUse { MustUse { value: 1 } }

fn main() -> i32 {
    let a = [make_linear(), make_linear()];  // ERROR: 'a' is dropped, not consumed
    0
}
```

{{ rule(id="3.8:39", cat="informative") }}

Linear types are useful for:
- Resources that must be explicitly released (file handles, database transactions)
- Protocol enforcement (ensuring state machine transitions are completed)
- Results that must be checked (similar to `must_use` attributes)

## Use After Move

{{ rule(id="3.8:5", cat="legality-rule") }}

It is a compile-time error to use (3.8:76) a value that has been moved.

{{ rule(id="3.8:6", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is moved
    let r = p;      // ERROR: use of moved value 'p'
    0
}
```

{{ rule(id="3.8:7", cat="normative") }}

A move type value is moved by any use of it (3.8:76). Assigning it to another binding, passing it as a by-value argument, and returning it from a function are all value-context occurrences, and are therefore all moves.

{{ rule(id="3.8:8", cat="example") }}

```rue
struct Data { value: i32 }

fn consume(d: Data) -> i32 { d.value }

fn main() -> i32 {
    let d = Data { value: 42 };
    let result = consume(d);  // d is moved into the function
    // d is no longer valid here
    result
}
```

## Copy Types and Multiple Uses

{{ rule(id="3.8:9", cat="normative") }}

A use of a Copy type (3.8:76) copies the value and leaves the original valid, so a Copy value may be used any number of times without being consumed.

{{ rule(id="3.8:10", cat="example") }}

```rue
fn main() -> i32 {
    let x = 42;
    let a = x;  // x is copied
    let b = x;  // x is copied again
    a + b       // 84
}
```

{{ rule(id="3.8:11", cat="normative") }}

Passing an argument by value is a use of it (3.8:76): a parameter of Copy type receives a copy of the argument, and a parameter of move type receives ownership by moving the argument.

## Partial Moves (Field-Level Moves)

{{ rule(id="3.8:22", cat="normative") }}

A use of a non-Copy field projection (3.8:76) is a partial move: only that specific field is moved, not the entire struct, and the sibling fields remain accessible.

{{ rule(id="3.8:23", cat="example") }}

```rue
struct Inner { x: i32 }
struct S { a: Inner, b: Inner }

fn main() -> i32 {
    let s = S { a: Inner { x: 1 }, b: Inner { x: 2 } };
    let x = s.a;   // Only s.a is moved
    let y = s.b;   // s.b is still valid
    x.x + y.x      // 3
}
```

{{ rule(id="3.8:24", cat="legality-rule") }}

It is a compile-time error to access a field that has already been moved.

{{ rule(id="3.8:25", cat="example") }}

```rue
struct Inner { x: i32 }
struct S { a: Inner, b: Inner }

fn main() -> i32 {
    let s = S { a: Inner { x: 1 }, b: Inner { x: 2 } };
    let x = s.a;   // s.a is moved
    let z = s.a;   // ERROR: use of moved value 's.a'
    0
}
```

{{ rule(id="3.8:26", cat="legality-rule") }}

A struct with any moved fields cannot be used as a whole value. It is a compile-time error to move or pass the struct after any of its non-Copy fields have been moved.

{{ rule(id="3.8:27", cat="example") }}

```rue
struct Inner { x: i32 }
struct S { a: Inner, b: Inner }

fn consume(s: S) -> i32 { s.a.x + s.b.x }

fn main() -> i32 {
    let s = S { a: Inner { x: 1 }, b: Inner { x: 2 } };
    let x = s.a;   // s.a is moved (partial move)
    consume(s)     // ERROR: use of moved value 's' (partially moved)
}
```

{{ rule(id="3.8:28", cat="normative") }}

A use of a Copy-type field (3.8:76) copies it and does not move it; Copy-type fields can therefore be accessed any number of times without affecting the struct's move state.

{{ rule(id="3.8:29", cat="example") }}

```rue
struct S { a: i32, b: i32 }

fn main() -> i32 {
    let s = S { a: 1, b: 2 };
    let x = s.a;   // s.a is copied
    let y = s.a;   // s.a can be copied again
    let z = s.b;   // s.b is also valid
    x + y + z      // 4
}
```

{{ rule(id="3.8:53", cat="legality-rule") }}

The base of a field projection is read in place context (3.8:76), but it must still own its storage. Accessing a field through a moved ancestor path is therefore a compile-time error even when the accessed field is itself a Copy type: the moved ancestor's storage is no longer owned by the variable, so nothing within it may be read.

{{ rule(id="3.8:54", cat="example") }}

```rue
struct Inner { x: i32 }
struct Outer { f: Inner }

fn consume(i: Inner) -> i32 { i.x }

fn main() -> i32 {
    let o = Outer { f: Inner { x: 1 } };
    let a = consume(o.f);  // o.f is moved
    let b = o.f.x;         // ERROR: use of moved value 'o.f.x'
    a + b
}
```

{{ rule(id="3.8:55", cat="normative") }}

Assigning a new value to a moved field reinitializes it. After the assignment, the field (and any of its subfields) may be used again.

{{ rule(id="3.8:56", cat="example") }}

```rue
struct Inner { x: i32 }
struct Outer { f: Inner }

fn consume(i: Inner) -> i32 { i.x }

fn main() -> i32 {
    let mut o = Outer { f: Inner { x: 1 } };
    let a = consume(o.f);     // o.f is moved
    o.f = Inner { x: 2 };     // o.f is reinitialized
    let b = o.f.x;            // OK: o.f is valid again
    a + b                     // 3
}
```

{{ rule(id="3.8:77", cat="legality-rule") }}

Assigning to a place whose type carries a linear value (3.8:57) is a compile-time error when the place currently holds a live value. A linear value **MUST** be consumed explicitly (3.8:32); an assignment that overwrote it would drop it implicitly (3.9:18), which linearity forbids. The assignment is legal only when the destination place has provably been moved out on every path reaching it — the reinitialization idiom (3.8:55): a moved-out place holds nothing to destroy. For an array whose element type carries a linear value, whole-array reassignment is legal only when every element has been consumed (as a whole or element-wise, 3.8:71); whole-array reinitialization is the only recovery path, because once any element has been moved out, assigning into the array — to an element or through an element — is itself an error (E0480, 3.8:72 and 7.1:46), including at the exact constant index that was moved out. An element reached through a non-constant (runtime) index can never be proven moved out and its assignment is always rejected. This restriction applies regardless of the run-time move state: the diagnostic is determined by the destination's type together with the statically tracked move paths, never by a run-time drop flag.

{{ rule(id="3.8:78", cat="example") }}

```rue
linear struct L { v: i32 }

fn remake(x: L) -> L { @drop(x); L { v: 9 } }

fn main() -> i32 {
    let mut x = L { v: 1 };
    x = L { v: 2 };   // ERROR: would overwrite a live linear value
    x = remake(x);    // OK: the right-hand side consumed x first
    @drop(x);
    0
}
```

## Array Element Moves

{{ rule(id="3.8:68", cat="normative") }}

Indexing an array variable with a compile-time constant index whose element type is not Copy moves that element out of the array. Only that element is invalidated; sibling elements remain usable and are still dropped normally. Element moves are tracked only for indexing applied directly to an array variable (or by-value array parameter); an array reached through another projection, or indexed with a non-constant index, cannot be moved out of (see the legality rule in the Arrays chapter).

{{ rule(id="3.8:69", cat="example") }}

```rue
struct Big { value: i32 }

fn consume(b: Big) -> i32 { b.value }

fn main() -> i32 {
    let xs = [Big { value: 1 }, Big { value: 2 }];
    let a = consume(xs[0]);  // moves only xs[0]
    let b = consume(xs[1]);  // xs[1] is still valid
    a + b                    // 3
}
```

{{ rule(id="3.8:70", cat="legality-rule") }}

While one or more elements of an array are moved out, it is a compile-time error to use the moved element (including reading a field through it), to use the array as a whole value, or to index the array with a non-constant index. The non-constant-index restriction is required for soundness: the compiler cannot know at compile time whether a runtime index denotes a moved-out element.

{{ rule(id="3.8:71", cat="normative") }}

An array whose elements carry linear values may be consumed element-wise: its must-consume obligation is satisfied when every element has been consumed on every non-diverging path — moved out as a whole (a constant-index move, 3.8:68), or, for an element whose type is a carrier struct linear only by infection (3.8:58), by consuming each of the element's linear sub-places (3.8:60). The sub-place route applies to an array anywhere in a place tree: the elements of an array reached through a field projection cannot be moved out as wholes (3.8:68), but consuming their linear sub-places (for example `h.arr[0].p` and `h.arr[1].p`) still discharges the array field's obligation. Consuming only some elements, or an element on only some paths, is a compile-time error naming the elements — or the sub-place — left unconsumed.

{{ rule(id="3.8:72", cat="legality-rule") }}

While one or more elements of an array are moved out, it is a compile-time error to assign into the array — to an element, or through an element (e.g. to a field of an element). Element writes do not reinstate per-element ownership; the whole array must be reinitialized instead, which makes every element owned (and droppable) again.

{{ rule(id="3.8:73", cat="dynamic-semantics") }}

At scope exit (and when an array variable is overwritten), elements that were moved out on every path reaching that point are not dropped; elements moved out on only some paths are dropped exactly when the executed path did not move them; untouched elements are dropped, in ascending index order.

{{ rule(id="3.8:74", cat="normative") }}

A zero-length array of a linear element type holds no linear values, so its must-consume obligation is vacuously satisfied: it may be dropped (as a local, a by-value parameter, or a discarded expression value) without error. This applies to any array shape whose total element count is zero (for example `[L; 0]`, `[[L; 5]; 0]`, and `[[L; 0]; 5]`). It does not apply to a linear struct itself: a value of a `linear struct` type must be consumed regardless of what its fields hold.

{{ rule(id="3.8:75", cat="example") }}

```rue
linear struct MustUse { value: i32 }

fn main() -> i32 {
    let _none: [MustUse; 0] = [];  // OK: nothing to consume
    0
}
```

## Shadowing and Moves

{{ rule(id="3.8:12", cat="normative") }}

Shadowing a variable does not prevent it from being moved. A moved variable remains invalid even if a new variable with the same name is introduced in an inner scope.

{{ rule(id="3.8:13", cat="example") }}

```rue
struct Data { value: i32 }

fn main() -> i32 {
    let d = Data { value: 1 };
    let x = d;  // d is moved
    {
        let d = Data { value: 2 };  // New 'd' shadows, but doesn't restore old 'd'
        d.value
    }
    // Original 'd' is still invalid here
}
```
