+++
title = "Structs"
weight = 7
template = "tutorial/page.html"
+++

# Structs

Structs let you create custom types by grouping related data together.

## Defining Structs

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };

    println(@to_string(origin.x));  // prints: 0
    println(@to_string(target.y));  // prints: 4

    0
}
```

## Structs in Functions

Pass structs to functions and return them:

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn distance_squared(p1: Point, p2: Point) -> i32 {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    dx * dx + dy * dy
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };

    let dist_sq = distance_squared(origin, target);
    println(@to_string(dist_sq));  // prints: 25 (distance is 5)

    dist_sq
}
```

## Nested Structs

Structs can contain other structs:

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

struct Rectangle {
    origin: Point,
    width: i32,
    height: i32,
}

fn area(rect: Rectangle) -> i32 {
    rect.width * rect.height
}

fn origin_x(borrow rect: Rectangle) -> i32 {
    rect.origin.x
}

fn main() -> i32 {
    let rect = Rectangle {
        origin: Point { x: 10, y: 20 },
        width: 100,
        height: 50,
    };

    println(@to_string(origin_x(borrow rect)));  // prints: 10
    println(@to_string(area(rect)));             // prints: 5000

    0
}
```

## Mutable Struct Fields

If a struct variable is mutable, you can modify its fields:

```rue check
const std = @import("std");

struct Counter {
    value: i32,
}

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value = c.value + 1;

    println(@to_string(c.value));  // prints: 2
    c.value
}
```

## Ownership: Moves and Copies

Integers and booleans are copied when you use them. Structs are different:
unless you opt in to copying, a struct value is *moved* when it is assigned or
passed by value. After a move, the old place no longer owns a live value.

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn use_point(p: Point) {
    println(@to_string(p.x));
}

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    use_point(p);     // p moves here
    // use_point(p);  // ERROR: value already moved

    0
}
```

The same rule applies to assignment:

```rue check
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    let p2 = p1;      // p1 moves into p2

    println(@to_string(p2.x));       // prints: 1
    // println(@to_string(p1.x));    // ERROR: p1 was moved

    0
}
```

If a struct is just data and duplicating it is safe, mark it with `@copy`.
Copies leave the source usable:

```rue check
const std = @import("std");

@copy
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    let mut p2 = p1;  // p2 is a copy; p1 is still valid

    p2.x = 100;

    println(@to_string(p1.x));  // prints: 1 (unchanged)
    println(@to_string(p2.x));  // prints: 100

    0
}
```

Use the default move behavior for values that represent ownership, resources, or
anything that should not be duplicated accidentally. Use `@copy` only for small
plain-data structs whose fields can all be copied.
