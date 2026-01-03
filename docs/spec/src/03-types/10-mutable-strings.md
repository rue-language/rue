+++
title = "Mutable Strings"
weight = 10
template = "spec/page.html"
+++

# Mutable Strings

This section describes the mutable string capabilities, building on the core `String` type from section 3.7.

{{ preview_feature(feature="mutable_strings", adr="ADR-0014") }}

## String Representation

{{ rule(id="3.10:1", cat="normative") }}

A `String` value consists of three components: a pointer to the string data, the length in bytes, and the allocated capacity.

{{ rule(id="3.10:2", cat="normative") }}

When capacity is zero, the string data points to read-only memory (a string literal). When capacity is greater than zero, the string data is heap-allocated and can be mutated.

{{ rule(id="3.10:3", cat="informative") }}

This representation allows string literals to remain cheap (no allocation) while enabling mutation when needed. Mutation methods automatically promote read-only strings to the heap.

## String Ownership

{{ rule(id="3.10:4", cat="normative") }}

`String` is an affine type: a `String` value is consumed when used and cannot be used again unless explicitly cloned.

{{ rule(id="3.10:5", cat="normative") }}

`String` is not `@copy`. Passing a string to a function or assigning it to another binding moves the string.

{{ rule(id="3.10:6", cat="example") }}

```rue
fn takes_string(s: String) -> i32 { 0 }

fn main() -> i32 {
    var s = "hello";
    takes_string(s);    // s is moved
    // takes_string(s); // ERROR: use of moved value
    0
}
```

## Construction

{{ rule(id="3.10:7", cat="normative") }}

`String::new()` returns an empty string with no allocation.

{{ rule(id="3.10:8", cat="normative") }}

`String::with_capacity(cap: u64)` returns an empty string with pre-allocated capacity for `cap` bytes.

{{ rule(id="3.10:9", cat="example") }}

```rue
fn main() -> i32 {
    let empty = String::new();
    let prealloc = String::with_capacity(1024);
    0
}
```

## Query Methods

{{ rule(id="3.10:10", cat="normative") }}

`fn len(borrow self) -> u64` returns the length of the string in bytes.

{{ rule(id="3.10:11", cat="normative") }}

`fn capacity(borrow self) -> u64` returns the allocated capacity of the string. Returns zero for string literals.

{{ rule(id="3.10:12", cat="normative") }}

`fn is_empty(borrow self) -> bool` returns true if the string length is zero.

{{ rule(id="3.10:13", cat="informative") }}

Query methods use `borrow self` to access the string without consuming it, leaving the string valid after the call.

{{ rule(id="3.10:14", cat="example") }}

```rue
fn main() -> i32 {
    let s = "hello";
    if s.len() == 5 && !s.is_empty() {
        0
    } else {
        1
    }
}
```

## Mutation Methods

{{ rule(id="3.10:15", cat="normative") }}

`fn push_str(inout self, other: String)` appends the contents of `other` to the string. If the string is a literal (capacity zero), it is first promoted to the heap.

{{ rule(id="3.10:16", cat="normative") }}

`fn push(inout self, byte: u8)` appends a single byte to the string.

{{ rule(id="3.10:17", cat="normative") }}

`fn clear(inout self)` removes all content from the string but retains the allocated capacity.

{{ rule(id="3.10:18", cat="normative") }}

`fn reserve(inout self, additional: u64)` ensures the string has capacity for at least `additional` more bytes.

{{ rule(id="3.10:19", cat="informative") }}

Mutation methods use `inout self` to modify the string in place. The variable must be declared with `var` to allow mutation.

{{ rule(id="3.10:20", cat="example") }}

```rue
fn main() -> i32 {
    var s = String::new();
    s.push_str("hello");
    s.push_str(" world");
    s.push(33);  // '!' character
    0
}
```

## Heap Promotion

{{ rule(id="3.10:21", cat="dynamic-semantics") }}

When a mutation method is called on a string literal (capacity zero), the string is promoted to the heap:
1. A heap buffer is allocated with capacity for the existing content plus the new content
2. The existing content is copied from read-only memory to the heap buffer
3. The string's pointer and capacity are updated
4. The mutation is performed

{{ rule(id="3.10:22", cat="informative") }}

Heap promotion is transparent to the user. There is no separate "owned" vs "borrowed" string distinction.

{{ rule(id="3.10:23", cat="example") }}

```rue
fn main() -> i32 {
    var s = "hello";     // literal: capacity = 0
    s.push_str("!");     // promotes to heap, then appends
    // s is now "hello!" with capacity > 0
    0
}
```

## Growth Strategy

{{ rule(id="3.10:24", cat="dynamic-semantics") }}

When appending would exceed the current capacity, a new buffer is allocated with double the current capacity (minimum 16 bytes). The existing content is copied and the old buffer is freed.

{{ rule(id="3.10:25", cat="informative") }}

The doubling growth strategy amortizes allocation cost over many appends, providing O(1) amortized time per append.

## Clone

{{ rule(id="3.10:26", cat="normative") }}

`fn clone(borrow self) -> String` creates a deep copy of the string, allocating a new heap buffer with the same content.

{{ rule(id="3.10:27", cat="informative") }}

Clone borrows `self` so the original string remains valid. Cloning always allocates, even for string literals.

{{ rule(id="3.10:28", cat="example") }}

```rue
fn main() -> i32 {
    let a = "hello";
    let b = a.clone();  // deep copy
    // Both a and b are valid
    0
}
```

## Destructor

{{ rule(id="3.10:29", cat="dynamic-semantics") }}

When a `String` value is dropped:
- If capacity is zero (literal), no action is taken
- If capacity is greater than zero (heap-allocated), the heap buffer is freed

{{ rule(id="3.10:30", cat="informative") }}

The destructor automatically distinguishes between string literals and heap-allocated strings, ensuring correct cleanup.

{{ rule(id="3.10:31", cat="example") }}

```rue
fn main() -> i32 {
    var s = "hello";
    s.push_str("!");  // promotes to heap
    0
}  // destructor frees the heap buffer
```

## Byte String Semantics

{{ rule(id="3.10:32", cat="informative") }}

Rue strings are *conventionally UTF-8* rather than strictly validated:
- String literals are valid UTF-8 (validated at compile time)
- At runtime, strings are byte sequences
- Methods like `push_str` accept any bytes
- No runtime UTF-8 validation overhead

{{ rule(id="3.10:33", cat="informative") }}

This approach matches Go's `string` and Rust's `bstr` crate: UTF-8 is the convention, but the type does not enforce it at runtime.
