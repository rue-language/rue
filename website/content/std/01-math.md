+++
title = "math"
weight = 1
+++

# std.math

The `math` module provides basic mathematical utilities for working with integers.

## Import

```rue
const math = @import("std").math;
```

## Functions

### abs

```rue
pub fn abs(x: i32) -> i32
```

Returns the absolute value of an integer.

**Parameters:**
- `x` - The input value

**Returns:** The absolute value of `x`

**Example:**

```rue
const math = @import("std").math;

fn main() -> i32 {
    let a = math.abs(-42);  // 42
    let b = math.abs(10);   // 10
    let c = math.abs(0);    // 0
    a
}
```

---

### min

```rue
pub fn min(a: i32, b: i32) -> i32
```

Returns the smaller of two values.

**Parameters:**
- `a` - First value
- `b` - Second value

**Returns:** The smaller of `a` and `b`

**Example:**

```rue
const math = @import("std").math;

fn main() -> i32 {
    let smaller = math.min(10, 20);  // 10
    let same = math.min(5, 5);       // 5
    smaller
}
```

---

### max

```rue
pub fn max(a: i32, b: i32) -> i32
```

Returns the larger of two values.

**Parameters:**
- `a` - First value
- `b` - Second value

**Returns:** The larger of `a` and `b`

**Example:**

```rue
const math = @import("std").math;

fn main() -> i32 {
    let larger = math.max(10, 20);  // 20
    let same = math.max(5, 5);      // 5
    larger
}
```

---

### clamp

```rue
pub fn clamp(x: i32, lo: i32, hi: i32) -> i32
```

Clamps a value to be within the given range `[lo, hi]`.

If `x` is less than `lo`, returns `lo`. If `x` is greater than `hi`, returns `hi`. Otherwise, returns `x`.

**Parameters:**
- `x` - The value to clamp
- `lo` - The lower bound (inclusive)
- `hi` - The upper bound (inclusive)

**Returns:** The clamped value

**Example:**

```rue
const math = @import("std").math;

fn main() -> i32 {
    let a = math.clamp(5, 0, 10);    // 5 (within range)
    let b = math.clamp(-5, 0, 10);   // 0 (clamped to lower bound)
    let c = math.clamp(15, 0, 10);   // 10 (clamped to upper bound)
    a + b + c  // 15
}
```

## Complete Example

Here's a more complete example using multiple math functions:

```rue
const math = @import("std").math;

fn distance(a: i32, b: i32) -> i32 {
    math.abs(a - b)
}

fn main() -> i32 {
    let x = 15;
    let y = 7;

    // Calculate distance between x and y
    let dist = distance(x, y);  // 8

    // Clamp the result to a valid range
    let clamped = math.clamp(dist, 0, 5);  // 5

    // Get the larger of the two original values
    let larger = math.max(x, y);  // 15

    clamped + larger  // 20
}
```

## Implementation

The math module is implemented in `std/math.rue`. All functions currently operate on `i32` values. Future versions may include overloads for other integer types.
