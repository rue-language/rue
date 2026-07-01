+++
title = "Move Semantics"
weight = 8
+++

# Move Semantics

This section describes how values are moved and copied in Rue.

## Value Categories

{{ rule(id="3.8:1", cat="normative") }}

Types in Rue are categorized by how they behave when used:
- **Copy types** can be implicitly duplicated when used. Using a Copy type does not consume the original value.
- **Move types** (also called affine types) are consumed when used. After using a move type value, the original binding becomes invalid.

{{ rule(id="3.8:2", cat="normative") }}

The following types are Copy types:
- All integer types (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`)
- The boolean type (`bool`)
- The unit type (`()`)
- Enum types (all variants of an enum)
- Array types `[T; N]` where `T` is a Copy type

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

A `@copy` struct **MAY** contain fields of primitive Copy types (integers, booleans, unit), enum types, arrays of Copy types, or other `@copy` struct types.

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

A linear value is consumed when it is:
- Passed as an argument to a function (the function is the consumer)
- Returned from a function (the caller becomes responsible for consuming it)
- Field access is performed on the value (the value is destructured)

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

{{ rule(id="3.8:51", cat="normative") }}

A control-flow path that diverges (for example, by executing a `return` expression) does not reach the end of the value's scope, and so is exempt from the consumption requirement on that path.

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

A type *carries a linear value* if it is a linear struct type, an array type whose element type carries a linear value, or a struct type with a field whose type carries a linear value. Pointer types do not carry a linear value (a pointer does not own its pointee).

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

It is a compile-time error for a field access that consumes a linear value (destructuring, 3.8:33) to implicitly drop a *different* field that carries a linear value. Every struct level along the access path is checked: at each level, all fields other than the accessed one are dropped by the destructure.

{{ rule(id="3.8:61", cat="example") }}

```rue
linear struct MustUse { value: i32 }

struct Container { inner: MustUse, tag: i32 }

fn sink(m: MustUse) -> i32 { m.value }

fn main() -> i32 {
    let c = Container { inner: MustUse { value: 1 }, tag: 2 };
    c.tag            // ERROR: would implicitly drop linear field 'inner'
}

fn ok() -> i32 {
    let c = Container { inner: MustUse { value: 1 }, tag: 2 };
    sink(c.inner)    // OK: extracts the linear field; 'tag' (non-linear) is dropped
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

## The `@handle` Directive

{{ rule(id="3.8:40", cat="normative") }}

A struct type **MAY** be declared as a handle type using the `@handle` directive before the struct definition. Handle types support explicit duplication via a `.handle()` method.

{{ rule(id="3.8:41", cat="syntax") }}

```ebnf
handle_struct = "@handle" struct_def ;
```

{{ rule(id="3.8:42", cat="normative") }}

A struct marked with `@handle` **MUST** provide a method named `handle` with the following signature:

```rue
fn handle(self) -> T
```

where `T` is the handle struct type. It is a compile-time error to mark a struct with `@handle` if it does not provide this method.

{{ rule(id="3.8:43", cat="legality-rule") }}

The `handle` method **MUST** take exactly one parameter (`self` of the struct type) and **MUST** return the same struct type. It is a compile-time error if the method signature differs.

{{ rule(id="3.8:44", cat="example") }}

```rue
@handle
struct Counter { count: i32 }

impl Counter {
    fn handle(self) -> Counter {
        Counter { count: self.count }
    }
}

fn main() -> i32 {
    let a = Counter { count: 1 };
    let b = a.handle();  // explicit duplication
    b.count
}
```

{{ rule(id="3.8:45", cat="normative") }}

Calling `.handle()` on a handle type does not consume the receiver and returns a new owned value. Both the original and the returned value are valid after the call.

{{ rule(id="3.8:46", cat="informative") }}

Handle types are useful for:
- Reference-counted types (Rc, Arc) where duplication increments the count
- Interned strings where duplication is cheap
- Shared resources where explicit duplication makes cost visible

{{ rule(id="3.8:47", cat="normative") }}

A `@copy` struct implicitly supports handle semantics. Any `@copy` type can be explicitly duplicated, although the `.handle()` method is not required.

{{ rule(id="3.8:48", cat="informative") }}

The difference between `@copy` and `@handle`:
- `@copy` types are duplicated implicitly when used
- `@handle` types require explicit `.handle()` calls for duplication
- `@copy` is appropriate for small, cheap-to-copy types (like `Point`)
- `@handle` is appropriate for types where duplication has visible cost (like reference-counted types)

{{ rule(id="3.8:49", cat="normative") }}

A linear struct **MAY** be marked with `@handle` if explicit duplication is meaningful (e.g., forking a transaction).

## Use After Move

{{ rule(id="3.8:5", cat="legality-rule") }}

It is a compile-time error to use a value that has been moved.

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

A value is considered moved when it is:
- Assigned to another variable
- Passed as an argument to a function
- Returned from a function

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

Copy types can be used multiple times without being consumed.

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

Function parameters of Copy types receive a copy of the argument. Function parameters of move types receive ownership of the argument.

## Partial Moves (Field-Level Moves)

{{ rule(id="3.8:22", cat="normative") }}

When a non-Copy field of a struct is accessed (moved out of), only that specific field is moved, not the entire struct. Other fields remain accessible.

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

Accessing Copy-type fields does not move them. Copy-type fields can be accessed any number of times without affecting the struct's move state.

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

Accessing a field through a moved ancestor path is a compile-time error even when the accessed field is itself a Copy type. The moved ancestor's storage is no longer owned by the variable, so nothing within it may be read.

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

An array whose elements carry linear values may be consumed element-wise: its must-consume obligation is satisfied when every element has been moved out on every non-diverging path. Moving out only some elements, or moving an element on only some paths, is a compile-time error naming the elements that are not consumed.

{{ rule(id="3.8:72", cat="legality-rule") }}

While one or more elements of an array are moved out, it is a compile-time error to assign into the array — to an element, or through an element (e.g. to a field of an element). Element writes do not reinstate per-element ownership; the whole array must be reinitialized instead, which makes every element owned (and droppable) again.

{{ rule(id="3.8:73", cat="dynamic-semantics") }}

At scope exit (and when an array variable is overwritten), elements that were moved out on every path reaching that point are not dropped; elements moved out on only some paths are dropped exactly when the executed path did not move them; untouched elements are dropped, in ascending index order.

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
