+++
title = "Destructors"
weight = 9
+++

# Destructors

This section describes when and how values are cleaned up in Rue.

## Drop Semantics

{{ rule(id="3.9:1", cat="normative") }}

When a value's owning binding goes out of scope and the value has not been moved elsewhere, the value is *dropped*. Dropping a value runs its destructor, if it has one.

{{ rule(id="3.9:2", cat="normative") }}

A value is dropped exactly once. Values that are moved are not dropped at their original binding site; they are dropped at their final destination.

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
- Enum types
- Arrays of trivially droppable types

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

{{ rule(id="3.9:14", cat="informative") }}

The distinction between "drop order of bindings" (reverse declaration) and "drop order of fields" (declaration order) matches C++ and Rust behavior. Bindings use LIFO for dependency correctness; fields use declaration order for consistency with construction order.

## Drop Placement

{{ rule(id="3.9:15", cat="dynamic-semantics") }}

Drops are inserted at the following points:
- At the end of a block scope, for all live bindings declared in that scope
- Before a `return` statement, for all live bindings in all enclosing scopes
- Before a `break` statement, for all live bindings declared inside the loop

{{ rule(id="3.9:16", cat="dynamic-semantics") }}

Each branch of a conditional independently drops bindings declared within that branch.

{{ rule(id="3.9:17", cat="example") }}

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
