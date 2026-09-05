+++
title = "Borrow and Inout"
weight = 9
template = "tutorial/page.html"
+++

# Borrow and Inout Parameters

When you read code, you want to understand what it does without tracing through every function. Rue helps by making mutation visible at the call site.

This chapter builds on ownership from the structs chapter. Passing a non-`@copy`
struct by value moves it into the callee. `borrow` and `inout` are how you let a
function access a value without taking ownership of it.

## Reading Code at a Glance

Look at this code:

```rue check
fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(inout values);
    values[0]
}

fn double_all(inout arr: [i32; 3]) {
    let mut i: u64 = 0;
    while i < 3 {
        arr[i] = arr[i] * 2;
        i = i + 1;
    }
}
```

Without seeing `double_all`'s definition, you already know it modifies `values`. The `inout` keyword tells you: this function will change my data.

Compare that to languages where mutation is invisible:

```go
// Go: does this modify values? You can't tell without reading sort()
sort.Ints(values)
```

```python
# Python: same problem
values.sort()
```

In Rue, mutation is always explicit at the call site.

## How It Works

### Inout: Modify in Place

Use `inout` when a function needs to modify its argument:

```rue check
const std = @import("std");

fn double_all(inout arr: [i32; 3]) {
    let mut i: u64 = 0;
    while i < 3 {
        arr[i] = arr[i] * 2;
        i = i + 1;
    }
}

fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(inout values);

    println(@to_string(values[0]));  // prints: 20
    println(@to_string(values[1]));  // prints: 40
    println(@to_string(values[2]));  // prints: 60

    values[0]
}
```

Both the function signature and the call site use `inout`. There's no way to accidentally miss that mutation is happening.

### Borrow: Read Without Copying

Use `borrow` when you want to read data without copying it:

```rue check
const std = @import("std");

fn sum_array(borrow arr: [i32; 5]) -> i32 {
    let mut total = 0;
    let mut i: u64 = 0;
    while i < 5 {
        total = total + arr[i];
        i = i + 1;
    }
    total
}

fn main() -> i32 {
    let numbers = [1, 2, 3, 4, 5];
    let sum = sum_array(borrow numbers);
    println(@to_string(sum));  // prints: 15
    sum
}
```

With `borrow`, you know the function won't change your data.

Borrowing also keeps non-copy structs usable after the call:

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn sum_point(borrow p: Point) -> i32 {
    p.x + p.y
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    let sum = sum_point(borrow p);

    println(@to_string(sum));  // prints: 30
    println(@to_string(p.x));  // still valid: p was borrowed, not moved

    sum
}
```

## Combining Them

You can mix borrow and inout in a single function:

```rue check
const std = @import("std");

fn copy_into(borrow src: [i32; 3], inout dst: [i32; 3]) {
    let mut i: u64 = 0;
    while i < 3 {
        dst[i] = src[i];
        i = i + 1;
    }
}

fn main() -> i32 {
    let source = [1, 2, 3];
    let mut dest = [0, 0, 0];

    copy_into(borrow source, inout dest);

    println(@to_string(dest[0]));  // prints: 1
    println(@to_string(dest[1]));  // prints: 2
    println(@to_string(dest[2]));  // prints: 3

    0
}
```

Reading the call site, you immediately know: `source` is read, `dest` is modified.

Rue enforces the same rule inside one call: any number of `borrow` accesses, or
one `inout` access, but not both for the same value at the same time.

```rue check
fn observe(borrow old: i32, inout new: i32) -> i32 {
    old + new
}

fn main() -> i32 {
    let mut x = 1;

    // ERROR: cannot borrow x while it is also passed as inout
    // observe(borrow x, inout x);

    0
}
```

Uncommenting the `observe` call makes the example invalid. The compiler rejects
it because the callee would have both read-only and mutable access to `x` during
the same call.

## With Structs

These work with any type:

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn translate(inout p: Point, dx: i32, dy: i32) {
    p.x = p.x + dx;
    p.y = p.y + dy;
}

fn print_point(borrow p: Point) {
    println(@to_string(p.x));
    println(@to_string(p.y));
}

fn main() -> i32 {
    let mut pos = Point { x: 10, y: 20 };

    print_point(borrow pos);     // prints: 10, 20
    translate(inout pos, 5, -3);
    print_point(borrow pos);     // prints: 15, 17

    0
}
```

## Destructors and Early Drop

Some move-only values need cleanup when they go out of scope. Define that cleanup
with a `drop fn` named after the struct:

```rue check
const std = @import("std");

struct Guard {
    value: i32,
}

drop fn Guard(self) {
    println(@to_string(self.value));
}

fn main() -> i32 {
    let _guard = Guard { value: 42 };
    println(@to_string(1));

    0
}  // prints: 42 when _guard is dropped
```

You can also drop a value early with `@drop`. Dropping consumes the value, so it
cannot be used afterward:

```rue check
const std = @import("std");

struct Guard {
    value: i32,
}

drop fn Guard(self) {
    println(@to_string(self.value));
}

fn main() -> i32 {
    let guard = Guard { value: 42 };
    @drop(guard);       // prints: 42 here

    // guard.value      // ERROR: guard was moved into @drop
    0
}
```

A type with a destructor represents ownership of cleanup work, so it should stay
move-only. Do not mark destructor-bearing types with `@copy`.
