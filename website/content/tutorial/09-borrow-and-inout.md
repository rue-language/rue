+++
title = "Borrow and Inout"
weight = 9
template = "tutorial/page.html"
+++

# Borrow and Inout Parameters

When you read code, you want to understand what it does without tracing through every function. Rue helps by making mutation visible at the call site.

## Reading Code at a Glance

Look at this code:

```rue
fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(inout values);
    values[0]
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

```rue
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

    @dbg(values[0]);  // prints: 20
    @dbg(values[1]);  // prints: 40
    @dbg(values[2]);  // prints: 60

    values[0]
}
```

Both the function signature and the call site use `inout`. There's no way to accidentally miss that mutation is happening.

### Borrow: Read Without Copying

Use `borrow` when you want to read data without copying it:

```rue
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
    @dbg(sum);  // prints: 15
    sum
}
```

With `borrow`, you know the function won't change your data.

## Combining Them

You can mix borrow and inout in a single function:

```rue
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

    @dbg(dest[0]);  // prints: 1
    @dbg(dest[1]);  // prints: 2
    @dbg(dest[2]);  // prints: 3

    0
}
```

Reading the call site, you immediately know: `source` is read, `dest` is modified.

## With Structs

These work with any type:

```rue
struct Point {
    x: i32,
    y: i32,
}

fn translate(inout p: Point, dx: i32, dy: i32) {
    p.x = p.x + dx;
    p.y = p.y + dy;
}

fn print_point(borrow p: Point) {
    @dbg(p.x);
    @dbg(p.y);
}

fn main() -> i32 {
    let mut pos = Point { x: 10, y: 20 };

    print_point(borrow pos);     // prints: 10, 20
    translate(inout pos, 5, -3);
    print_point(borrow pos);     // prints: 15, 17

    0
}
```
