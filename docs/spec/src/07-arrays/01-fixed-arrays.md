+++
title = "Fixed-Size Arrays"
weight = 1
template = "spec/page.html"
+++

# Fixed-Size Arrays

## Array Literals

{{ rule(id="7.1:1", cat="normative") }}

```ebnf
array_literal = "[" [ expression { "," expression } ] "]" ;
```

{{ rule(id="7.1:2", cat="normative") }}

An array literal `[e0, …, e_{n-1}]` evaluates its element expressions left to
right and evaluates to an array value of type `[T; n]` that owns those `n`
elements in index order; every element is a value-context use of the one shared
element type `T` (core calculus `docs/formal/01-core-calculus.md` §5.8, rule
`(Array-Intro)`, and the reduction `(D-Array)` of §6.5).

{{ rule(id="7.1:3", cat="legality-rule") }}

All elements **MUST** have the same type.

{{ rule(id="7.1:4", cat="legality-rule") }}

The number of elements **MUST** match the declared array size.

{{ rule(id="7.1:5") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // 42
}
```

## Array-Repeat Literals

{{ rule(id="7.1:36", cat="normative") }}

```ebnf
array_literal = "[" ( [ expression { "," expression } ]
                    | expression ";" array_length ) "]" ;
```

An array literal has two forms. The *list form* `[e0, e1, …]` gives each
element explicitly. The *repeat form* `[value; count]` constructs an array of
`count` elements, each equal to `value`. The result type is `[T; count]` where
`T` is the type of `value`.

{{ rule(id="7.1:37", cat="legality-rule") }}

In the repeat form, `count` **MUST** be a non-negative integer compile-time
constant — an integer literal or an identifier naming a compile-time constant
(a file-level `const` or an in-scope `comptime` value parameter), resolved by
the same rules as an array-type length.

{{ rule(id="7.1:38", cat="legality-rule") }}

The element type of a repeat literal **MUST** be `Copy`. A repeat literal
materializes `count` copies of a single value, which is only well-defined when
the value can be copied; a non-Copy element type is a compile-time error.

{{ rule(id="7.1:39", cat="dynamic-semantics") }}

The `value` expression is evaluated exactly once; its result is copied into
each of the `count` array slots.

{{ rule(id="7.1:40") }}

```rue
fn main() -> i32 {
    let zeros: [i32; 128] = [0; 128];   // 128 copies of 0
    let sevens = [7; 3];                 // [7, 7, 7]
    zeros[0] + sevens[0] + sevens[1] + sevens[2]  // 21
}
```

## Array Indexing

{{ rule(id="7.1:6", cat="normative") }}

An index expression `arr[i]` denotes the place of the element at position `i`.
Used in value context it evaluates to that element — the value stored at that
position, copied out for a `Copy` element type and moved out for a move type
(core calculus `docs/formal/01-core-calculus.md` §6.5, rule `(D-Index)`:
`[v0, …, v_{n-1}][i]` yields `vi` once `i` is in range, which §6.3 then copies or
moves; the move case is governed by 7.1:28).

{{ rule(id="7.1:7", cat="legality-rule") }}

The index **MUST** be an integer type.

{{ rule(id="7.1:8") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [100, 42, 200];
    arr[1]  // 42
}
```

## Bounds Checking

{{ rule(id="7.1:9", cat="legality-rule") }}

For constant indices, bounds **MUST** be checked at compile time. A constant
index that is out of range is rejected during compilation and so never reaches
the runtime check (core calculus `docs/formal/01-core-calculus.md` §6.5; see
also 8.2:3).

{{ rule(id="7.1:10", cat="dynamic-semantics") }}

For variable indices, an out-of-range access **MUST** trap as if the bound were
tested at the moment the index navigates into the array, before the element is
read (core calculus `docs/formal/01-core-calculus.md` §6.5: the check precedes
the projection that reads the element; see also 8.2:5). This constrains
observable behavior, not emitted code: an implementation may omit or move the
dynamic test where it proves the trap behavior unchanged, subject to 8.2:9.

{{ rule(id="7.1:11", cat="dynamic-semantics") }}

A read whose index is out of range — `i` negative or `i ≥ n` for an `[T; n]` —
**MUST** trap: the bounds check that guards the projection fails, abandoning
evaluation to the `bounds` trap and halting the program with exit code 101 (core
calculus `docs/formal/01-core-calculus.md` §6.5, rule `(D-Index-Trap)`, and the
`bounds` trap category of §6.12; see also 8.2:1–8.2:2).

## Mutable Arrays

{{ rule(id="7.1:12", cat="normative") }}

Mutable arrays allow element assignment.

{{ rule(id="7.1:13") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // 42
}
```

## Array Type Syntax

{{ rule(id="7.1:14", cat="normative") }}

```ebnf
array_type   = "[" type ";" array_length "]" ;
array_length = INTEGER | IDENTIFIER | length_call ;
length_call  = IDENTIFIER "(" [ array_length { "," array_length } ] ")" ;
```

{{ rule(id="7.1:15", cat="legality-rule") }}

The length **MUST** be a non-negative integer value known at compile time. It is
either an integer literal, an identifier naming a compile-time constant — a
file-level `const` or an in-scope `comptime` value parameter — or a call to a
comptime-evaluable function (rule 7.1:41).

## Compile-Time Array Lengths

{{ rule(id="7.1:32", cat="normative") }}

An array length **MAY** be an identifier naming a compile-time constant in
addition to an integer literal. A file-level `const` of an integer type and a
`comptime` value parameter both qualify. The identifier is resolved by ordinary
scoped resolution (rule 10.3:8): a bare name in these positions is resolved in
the declaration or body scope in which the length appears — a `const` of that
same file, or a `comptime` value parameter in scope there — and a `comptime`
value parameter takes precedence over a same-named file-level `const`.
Declarations outside that scope do not participate merely because they are
globally unique: a constant declared only in another module is reached by
binding it into this file with a file-level `const`, and adding a same-named
constant in an unrelated module never changes which length a bare name
resolves to.

{{ rule(id="7.1:33", cat="legality-rule") }}

A named array length **MUST** resolve to a non-negative integer compile-time
constant. A runtime variable, a negative value, or a name that does not resolve
in scope — including a name that names a constant only in another module (rule
10.3:8) — in length position is a compile-time error.

{{ rule(id="7.1:34", cat="normative") }}

When an array length names a `comptime` value parameter, the length is resolved
at each specialization. The same generic definition therefore yields arrays of
different sizes for different argument values, so a `comptime` value parameter
can parameterize a type's memory layout — for example a fixed-capacity buffer or
stack — and not only its behavior.

{{ rule(id="7.1:35") }}

```rue
fn Buffer(comptime N: i32) -> type {
    struct { data: [i32; N], len: u32 }
}

fn main() -> i32 {
    let B2 = Buffer(2);
    let B4 = Buffer(4);
    let b2: B2 = B2 { data: [1, 2], len: 2 };
    let b4: B4 = B4 { data: [10, 20, 30, 40], len: 4 };
    b2.data[1] + b4.data[3]  // 42
}
```

{{ rule(id="7.1:41", cat="normative") }}

An array length **MAY** also be a call to a comptime-evaluable function, in
addition to a literal or a named constant. The callee **MUST** be a
value-returning function whose parameters are all `comptime` (the same
implicit-comptime shape that makes a call foldable in a `comptime` context);
its arguments are themselves array-length forms (a literal, a named constant,
or a nested call). The call is folded to the concrete length using the same
compile-time evaluator that reduces `comptime` blocks, and its result **MUST**
be a non-negative integer. A call whose callee takes a runtime parameter, is
nullary, returns a type, or names no known function is not a compile-time
length and is a compile-time error.

{{ rule(id="7.1:42") }}

```rue
fn fact(comptime n: i32) -> i32 {
    if n <= 1 { 1 } else { n * fact(n - 1) }
}

fn main() -> i32 {
    let a: [i32; fact(4)] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
        13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24
    ];  // length is fact(4) = 24
    a[23]  // 24
}
```

## Nested Arrays

{{ rule(id="7.1:16", cat="normative") }}

Arrays **MAY** contain other arrays as elements, forming multi-dimensional arrays.

{{ rule(id="7.1:17", cat="normative") }}

Nested arrays are indexed using chained bracket notation, evaluated left to right.

{{ rule(id="7.1:18") }}

```rue
fn main() -> i32 {
    let matrix: [[i32; 2]; 2] = [[1, 2], [3, 4]];
    matrix[1][1]  // 4
}
```

## Arrays in Structs

{{ rule(id="7.1:19", cat="normative") }}

Struct fields **MAY** have array types.

{{ rule(id="7.1:20", cat="normative") }}

Array fields are accessed by combining field access with array indexing.

{{ rule(id="7.1:21") }}

```rue
struct Container { values: [i32; 3] }

fn main() -> i32 {
    let c = Container { values: [10, 20, 30] };
    c.values[1]  // 20
}
```

## Arrays as Function Parameters

{{ rule(id="7.1:22", cat="normative") }}

Functions **MAY** accept arrays as parameters.

{{ rule(id="7.1:23", cat="normative") }}

Array parameters are passed by value. What that does to the argument follows the
element type's value category (3.8:2): `[T; N]` is a Copy type when `T` is, so the
whole array is copied and the caller's array stays valid; when `T` is a move type
the array is a move type too, so the by-value argument *moves* the whole array
(3.8:7) and the caller's array is invalid afterwards.

{{ rule(id="7.1:24") }}

```rue
fn sum(arr: [i32; 3]) -> i32 {
    arr[0] + arr[1] + arr[2]
}

fn main() -> i32 {
    let data: [i32; 3] = [10, 20, 12];
    sum(data)  // 42
}
```

## Array Projection Semantics

{{ rule(id="7.1:25", cat="normative") }}

Array indexing operates as a projection. Reading an element does not move the array itself.

{{ rule(id="7.1:26", cat="normative") }}

When reading an element of a Copy type (e.g., integers, booleans), the element is copied out.

{{ rule(id="7.1:27") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 30];
    let x = arr[0];     // i32 is Copy, so x is a copy
    let y = arr[0];     // Can read same element again
    x + y               // 20
}
```

{{ rule(id="7.1:28", cat="legality-rule") }}

When reading an element of a non-Copy type, the read moves the element out of the array only when the index is a compile-time constant and the indexing applies directly to an array variable (per-element move tracking; see Array Element Moves in the Move Semantics chapter). Any other such read — a non-constant index, or an array reached through another projection or computed by an expression — is a compile-time error: with a runtime index the compiler cannot know which element moved, so neither use-after-move checking nor drop elaboration could remain sound.

{{ rule(id="7.1:29") }}

```rue
struct BigThing { value: i32 }

fn first_index() -> u64 { 0 }

fn main() -> i32 {
    let arr: [BigThing; 2] = [BigThing { value: 1 }, BigThing { value: 2 }];
    let x = arr[0];             // OK: constant index moves arr[0] out
    let i = first_index();
    let y = arr[i];             // ERROR: cannot move out of indexed position
    x.value
}
```

{{ rule(id="7.1:30", cat="normative") }}

Array element assignment is an in-place mutation. It modifies the array without moving it.

{{ rule(id="7.1:31") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 3] = [1, 2, 3];
    arr[0] = 10;        // Mutates in place
    arr[1] = 20;        // Another mutation
    arr[0] + arr[1]     // 30
}
```

## Array Element Moves

The general move framework — value versus place context, and the copy-versus-move
effect of a *use* — is defined in the Move Semantics chapter (3.8:76). The rules in
this section are its array-chapter statement, and are the legality rules referenced
from that chapter (3.8:68).

{{ rule(id="7.1:43", cat="normative") }}

A non-`Copy` array element **MAY** be moved out of an array. Using `arr[i]` in a
value context moves the indexed element out of the array — but, per 7.1:28, only
when `i` is a compile-time constant and the indexing applies directly to an array
variable or by-value array parameter (a *constant-index move*). The move is a
partial move: only the indexed element is invalidated, and the sibling elements
remain usable (3.8:68).

{{ rule(id="7.1:44") }}

```rue
struct Big { value: i32 }

fn consume(b: Big) -> i32 { b.value }

fn main() -> i32 {
    let xs = [Big { value: 40 }, Big { value: 2 }];
    let a = consume(xs[0]);   // moves element 0 out
    let b = consume(xs[1]);   // sibling element 1 still usable
    a + b                     // 42
}
```

{{ rule(id="7.1:45", cat="legality-rule") }}

While one or more elements of an array have been moved out, it is a compile-time
error to use the array as a whole value, to use a moved-out element (including
reading a field through it), or to index the array with a non-constant index. The
non-constant-index restriction is required for soundness: with a runtime index the
compiler cannot know at compile time which element was moved. The sibling elements
that were not moved remain usable through constant-index projection (3.8:70).

{{ rule(id="7.1:46", cat="legality-rule") }}

While one or more elements of an array have been moved out, it is a compile-time
error to assign into the array — to an element (`arr[k] = …`) or through an element
(`arr[k].f = …`). An element write does not reinstate per-element ownership; the
whole array **MUST** be reinitialized (`arr = [ … ]`) instead, which makes every
element owned — and therefore droppable — again (3.8:72).

{{ rule(id="7.1:47", cat="normative") }}

An array whose element type carries a must-consume (linear) value satisfies its
consumption obligation when every element has been consumed on every non-diverging
path — moved out as a whole, or (for a carrier-struct element) by consuming each of
the element's linear sub-places, which is the only element-wise route for an array
reached through a field projection (3.8:71). Consuming only some elements, or an
element on only some paths, is a compile-time error naming what remains unconsumed
(3.8:71).

{{ rule(id="7.1:48", cat="dynamic-semantics") }}

At scope exit — and when an array variable is overwritten — elements that were moved
out on every path reaching that point are not dropped; an element moved out on only
some paths is dropped exactly on the paths that did not move it; untouched elements
are dropped in ascending index order (3.8:73).
