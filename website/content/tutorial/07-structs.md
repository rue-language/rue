+++
title = "Structs"
weight = 7
template = "tutorial/page.html"
+++

# Structs

Structs let you create custom types by grouping related data together.

## Defining Structs

```rue
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };

    @dbg(origin.x);  // prints: 0
    @dbg(target.y);  // prints: 4

    0
}
```

## Structs in Functions

Pass structs to functions and return them:

```rue
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
    @dbg(dist_sq);  // prints: 25 (distance is 5)

    dist_sq
}
```

## Nested Structs

Structs can contain other structs:

```rue
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

fn main() -> i32 {
    let rect = Rectangle {
        origin: Point { x: 10, y: 20 },
        width: 100,
        height: 50,
    };

    @dbg(area(rect));          // prints: 5000
    @dbg(rect.origin.x);       // prints: 10

    0
}
```

## Mutable Struct Fields

If a struct variable is mutable, you can modify its fields:

```rue
struct Counter {
    value: i32,
}

fn main() -> i32 {
    let mut c = Counter { value: 0 };
    c.value = c.value + 1;
    c.value = c.value + 1;

    @dbg(c.value);  // prints: 2
    c.value
}
```

## Move Semantics

By default, structs *move* when assigned or passed to functions. After a move, the original variable can't be used:

```rue
struct Point {
    x: i32,
    y: i32,
}

fn use_point(p: Point) {
    @dbg(p.x);
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    use_point(p1);    // p1 moves here
    // use_point(p1); // ERROR: value already moved

    0
}
```

If you want a type to be copyable instead, mark it with `@copy`:

```rue
@copy
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    let mut p2 = p1;  // p2 is a copy of p1

    p2.x = 100;

    @dbg(p1.x);  // prints: 1 (unchanged)
    @dbg(p2.x);  // prints: 100

    0
}
```
