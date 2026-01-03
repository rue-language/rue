+++
title = "Arrays"
weight = 6
template = "tutorial/page.html"
+++

# Arrays

Rue has fixed-size arrays with bounds checking at runtime.

## Creating Arrays

```rue
fn main() -> i32 {
    let numbers = [10, 20, 30, 40, 50];

    // Access by index
    @dbg(numbers[0]);  // prints: 10
    @dbg(numbers[4]);  // prints: 50

    numbers[0]
}
```

Array indices are zero-based and must be `u64`.

## Array Types

The type of an array includes its element type and length:

```rue
fn main() -> i32 {
    let a: [i32; 3] = [1, 2, 3];     // 3 elements
    let b: [bool; 2] = [true, false]; // 2 booleans

    @dbg(a[0]);
    0
}
```

## Iterating Over Arrays

Use a while loop with an index:

```rue
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

```rue
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

Rue checks array bounds at runtime. Accessing an invalid index causes a panic:

```rue
fn main() -> i32 {
    let arr = [1, 2, 3];
    @dbg(arr[10]);  // Runtime panic: index out of bounds
    0
}
```

This prevents memory safety bugs common in C and C++.

## Example: Finding Maximum

```rue
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
