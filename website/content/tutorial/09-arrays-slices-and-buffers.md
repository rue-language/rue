+++
title = "Arrays, Slices, and Buffers"
weight = 9
template = "tutorial/page.html"
+++

# Arrays, Slices, and Buffers

Rue has three ways to hold a sequence of values, and they form a ladder:

- a **fixed array** `[T; N]` whose length is part of its type;
- a **slice** `[T]`, a borrowed view of any array, that exists only as a
  function parameter;
- a **growable buffer** `ArrayBuf(T)` from the standard library, which owns
  heap memory and can change length at runtime.

```rue run
const std = @import("std");
const Ints = std.arraybuf.ArrayBuf(i64);

fn total(borrow xs: [i64]) -> i64 {
    let mut sum: i64 = 0;
    let mut i: u64 = 0;
    while i < xs.len() {
        sum += xs[i];
        i += 1;
    }
    sum
}

fn main() -> i32 {
    let fixed: [i64; 4] = [1, 2, 3, 4];
    println("fixed total = " + @to_string(total(borrow fixed)));

    let mut growable = Ints.new();
    let mut n: i64 = 1;
    while n <= 100 {
        growable.push(n);
        n += 1;
    }
    println("growable holds " + @to_string(growable.len()) + " values");
    0
}
```

```text
fixed total = 10
growable holds 100 values
```

## Fixed arrays

An array literal lists its elements. The type `[i32; 3]` reads "three
`i32`s", and `[i32; 3]` and `[i32; 5]` are different types.

```rue run
const std = @import("std");

fn main() -> i32 {
    let numbers = [10, 20, 30, 40, 50];
    let flags: [bool; 2] = [true, false];
    let zeros = [0; 8];  // eight zeros
    println(@to_string(numbers[0]) + " " + @to_string(numbers[4]));
    if flags[1] {
        println("flag set");
    }
    println(@to_string(zeros[7]));
    0
}
```

```text
10 50
0
```

Indices start at zero. The index expression must be an unsigned integer; an
untyped literal like `numbers[4]` works, and a loop counter should be declared
`u64`, the type of array lengths.

The best way to visit every element is `for`, which needs no index at all:

```rue run
const std = @import("std");

fn main() -> i32 {
    let numbers = [64, 34, 25, 12, 22];
    let mut max = numbers[0];
    for n in numbers {
        if n > max {
            max = n;
        }
    }
    println("max = " + @to_string(max));
    0
}
```

```text
max = 64
```

Arrays declared `let mut` can be assigned element by element:

```rue run
const std = @import("std");

fn main() -> i32 {
    let mut scores = [0, 0, 0];
    scores[0] = 100;
    scores[1] = 85;
    scores[2] = 92;
    let mut sum = 0;
    for s in scores {
        sum += s;
    }
    println("average = " + @to_string(sum / 3));
    0
}
```

```text
average = 92
```

## Bounds are checked

An index the compiler can see is out of range is rejected before the program
runs:

```rue compile-fail E0902
fn main() -> i32 {
    let arr = [1, 2, 3];
    arr[10]
}
```

```text
error: [E0902]: index out of bounds
```

An index it cannot see is checked at runtime. Reading past the end is a trap:
the program prints `error: index out of bounds` and exits with status `101`
instead of reading whatever memory happened to be there.

```rue run exit=101
const std = @import("std");

fn main() -> i32 {
    let xs = [1, 2, 3];
    let mut i: u64 = 0;
    let mut total = 0;
    while i < 4 {
        println("reading index " + @to_string(i));
        total += xs[i];
        i += 1;
    }
    total
}
```

```text
reading index 0
reading index 1
reading index 2
reading index 3
```

A `for` loop can never index out of range, which is one more reason to prefer
it.

## Slices

A function that takes `[i64; 3]` cannot be called with a `[i64; 5]`. To write
a function over "an array of any length", take a slice: `borrow xs: [T]`. The
caller passes any fixed array with `borrow`, and inside the function `xs.len()`
gives the length at runtime.

```rue run
const std = @import("std");

fn largest(borrow xs: [i64]) -> i64 {
    let mut best = xs[0];
    let mut i: u64 = 1;
    while i < xs.len() {
        if xs[i] > best {
            best = xs[i];
        }
        i += 1;
    }
    best
}

fn main() -> i32 {
    let three: [i64; 3] = [3, 9, 4];
    let five: [i64; 5] = [1, 2, 3, 4, 5];
    println(@to_string(largest(borrow three)) + " " + @to_string(largest(borrow five)));
    0
}
```

```text
9 5
```

Slices are the newest rung of the ladder, and today they only work for
elements that are 64 bits wide, such as `i64`, `u64`, and `f64`. A `[i32]`
parameter is rejected with a diagnostic that says so. That restriction is
expected to lift.

A slice is a *view*: it does not own its elements, and it is only ever a
parameter. You cannot return one, store one in a struct, or bind one to a
local. That restriction is what lets Rue have slices with no lifetime
annotations, because a view that cannot escape the call cannot outlive the
array it views:

```rue compile-fail E0487
fn first_part(borrow xs: [i64]) -> [i64] {
    xs
}

fn main() -> i32 {
    0
}
```

```text
error: [E0487]: a slice type `[T]` cannot be returned: slices are second-class views valid only in argument position
```

## `ArrayBuf`: a growable buffer

When the number of elements is only known at runtime, use `ArrayBuf(T)` from
`std.arraybuf`. Like `Option`, it is a function from a type to a type, so bind
the instantiation to a name.

```rue run
const std = @import("std");
const Ints = std.arraybuf.ArrayBuf(i32);
const OptI32 = std.option.Option(i32);

fn main() -> i32 {
    let mut values = Ints.new();
    values.push(10);
    values.push(20);
    values.push(30);
    println("len = " + @to_string(values.len()));

    match values.get(1) {
        OptI32.Some(v) => println("values[1] = " + @to_string(v)),
        OptI32.None => println("no values[1]"),
    }
    match values.get(7) {
        OptI32.Some(v) => println("values[7] = " + @to_string(v)),
        OptI32.None => println("no values[7]"),
    }

    match values.pop() {
        OptI32.Some(v) => println("popped " + @to_string(v)),
        OptI32.None => println("nothing to pop"),
    }
    println("len = " + @to_string(values.len()));
    0
}
```

```text
len = 3
values[1] = 20
no values[7]
popped 30
len = 2
```

`get` returns an `Option` rather than trapping, so a missing index is a value
you handle. `get_or(i, default)` is the shortcut when a fallback value makes
sense. `push`, `pop`, `set`, `len`, `first`, `last`, `contains`, and `clear`
are the rest of the everyday API.

An `ArrayBuf` owns a heap allocation and frees it in its destructor, exactly
as chapter 8 described. It is a move type, so pass it `borrow` or `inout`:

```rue run
const std = @import("std");
const Ints = std.arraybuf.ArrayBuf(i32);

fn fill(inout buf: Ints, count: i32) {
    let mut i = 0;
    while i < count {
        buf.push(i * i);
        i += 1;
    }
}

fn sum(borrow buf: Ints) -> i32 {
    let mut total = 0;
    let mut i: u64 = 0;
    while i < buf.len() {
        total += buf.get_or(i, 0);
        i += 1;
    }
    total
}

fn main() -> i32 {
    let mut squares = Ints.new();
    fill(inout squares, 5);
    println("sum of squares = " + @to_string(sum(borrow squares)));
    0
}
```

```text
sum of squares = 30
```

`for` does not iterate an `ArrayBuf` yet; an index loop over `len()` is the
current idiom.

## Choosing a rung

Use a fixed array when the size is known when you write the code. Take a slice
when a function should work on any array. Use `ArrayBuf` when the program
discovers the size as it runs. All three are cheap, and the compiler stops you
from confusing them.

Next: the same ladder for text.
