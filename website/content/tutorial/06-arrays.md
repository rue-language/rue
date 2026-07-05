+++
title = "Arrays"
weight = 6
template = "tutorial/page.html"
+++

# Arrays

Rue has fixed-size arrays with bounds checking at runtime.

## Creating Arrays

```rue check
fn main() -> i32 {
    let numbers = [10, 20, 30, 40, 50];

    // Access by index
    @dbg(numbers[0]);  // prints: 10
    @dbg(numbers[4]);  // prints: 50

    numbers[0]
}
```

Array indices are zero-based and must have an integer type. Loop examples often
use `u64` because lengths are represented as `u64`.

## Array Types

The type of an array includes its element type and length:

```rue check
fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];     // 3 elements
    let b: [bool; 2] = [true, false]; // 2 booleans

    @dbg(a[0]);
    0
}
```

## Iterating Over Arrays

Use a while loop with an index:

```rue check
fn main() -> i32 {
    let numbers = [10, 20, 30, 40, 50];

    let mut sum = 0;
    let mut i: u64 = 0;
    while i < 5 {
        sum = sum + numbers[i];
        i = i + 1;
    }
    @dbg(sum);  // prints: 150

    sum
}
```

## Mutable Arrays

Arrays are mutable if declared with `let mut`:

```rue check
fn main() -> i32 {
    let mut scores = [0, 0, 0];
    scores[0] = 100;
    scores[1] = 85;
    scores[2] = 92;

    @dbg(scores[0] + scores[1] + scores[2]);  // prints: 277
    0
}
```

## Bounds Checking

Rue checks array bounds. A constant out-of-bounds index is rejected at compile time:

```rue compile-fail
fn main() -> i32 {
    let arr = [1, 2, 3];
    @dbg(arr[10]);  // Error: index out of bounds
    0
}
```

Dynamic out-of-bounds indexes are checked at runtime. Together, these checks
prevent memory safety bugs common in C and C++.

## Example: Finding Maximum

```rue check
fn main() -> i32 {
    let numbers = [64, 34, 25, 12, 22];

    let mut max = numbers[0];
    let mut i: u64 = 1;
    while i < 5 {
        if numbers[i] > max {
            max = numbers[i];
        }
        i = i + 1;
    }

    @dbg(max);  // prints: 64
    max
}
```

## Fixed Arrays vs Growable Buffers

Fixed arrays are best when the length is known at compile time. Their length is
part of the type: `[i32; 3]` and `[i32; 5]` are different types.

When you need a collection that grows at runtime, use the standard library's
`ArrayBuf(T)`. Import the standard library explicitly and access the buffer
through the `std.arraybuf` namespace:

```rue check
const std = @import("std");

fn main() -> i32 {
    let Buffer = std.arraybuf.ArrayBuf(i32);
    let MaybeI32 = std.option.Option(i32);

    let mut values = Buffer::new();
    values.push(10);
    values.push(20);
    values.push(30);

    println("len = " + @to_string(values.len()));

    let second = match values.get(1) {
        MaybeI32::Some(n) => n,
        MaybeI32::None => -1,
    };
    println("second = " + @to_string(second));

    second
}
```

`ArrayBuf(i32)` owns heap storage and frees it automatically when the buffer is
dropped. Its `get` method returns `Option(i32)` rather than trapping:

- `Option::Some(value)` means the index was in bounds.
- `Option::None` means there was no element at that index.

Use fixed arrays for small, known-size data and `ArrayBuf(T)` when the program
discovers the number of elements as it runs. The slice and growable string parts
of Rue's collection/string design are still in progress, so this tutorial keeps
to the implemented fixed-array and `ArrayBuf` path.
