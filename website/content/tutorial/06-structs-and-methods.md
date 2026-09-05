+++
title = "Structs and Methods"
weight = 6
template = "tutorial/page.html"
+++

# Structs and Methods

A struct groups named fields into one type. Functions declared inside the
struct are its methods.

```rue run
const std = @import("std");

struct Counter {
    value: i32,
    step: i32,

    fn new(step: i32) -> Self {
        Self { value: 0, step: step }
    }

    fn bump(inout self) {
        self.value += self.step;
    }

    fn get(borrow self) -> i32 {
        self.value
    }
}

fn main() -> i32 {
    let mut c = Counter.new(5);
    c.bump();
    c.bump();
    println("counter = " + @to_string(c.get()));
    0
}
```

```text
counter = 10
```

## Defining and building structs

A struct declaration lists fields with their types. A struct *literal* names
the type and gives every field a value; there are no defaults, and leaving one
out is a compile error.

```rue run
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };
    println(@to_string(origin.x) + "," + @to_string(target.y));
    0
}
```

```text
0,4
```

Fields are read with `.`. If the binding is `let mut`, fields can be assigned:

```rue run
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let mut p = Point { x: 1, y: 1 };
    p.x = 10;
    p.y += 1;
    println(@to_string(p.x) + "," + @to_string(p.y));
    0
}
```

```text
10,2
```

Structs can contain other structs:

```rue run
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

fn main() -> i32 {
    let rect = Rectangle {
        origin: Point { x: 10, y: 20 },
        width: 100,
        height: 50,
    };
    println("origin.x = " + @to_string(rect.origin.x));
    println("area = " + @to_string(rect.width * rect.height));
    0
}
```

```text
origin.x = 10
area = 5000
```

## Methods

A function written inside the struct body, after the fields, is a method. Its
first parameter is `self`, the value the method is called on, and inside the
struct the type can be spelled `Self`.

The mode on `self` says what the method does to the value:

- `borrow self` reads it. This is the common case for getters and queries.
- `inout self` mutates it. The caller's binding must be `let mut`.
- plain `self` consumes it: the value moves into the method. Chapter 8 explains
  moves.

```rue run
const std = @import("std");

struct Rectangle {
    width: i32,
    height: i32,

    fn area(borrow self) -> i32 {
        self.width * self.height
    }

    fn scale(inout self, factor: i32) {
        self.width *= factor;
        self.height *= factor;
    }
}

fn main() -> i32 {
    let mut r = Rectangle { width: 3, height: 4 };
    println(@to_string(r.area()));
    r.scale(2);
    println(@to_string(r.area()));
    0
}
```

```text
12
48
```

Notice that the call site `r.scale(2)` does not spell out `inout`. The mode is
part of the method's declaration, and calling a mutating method on a binding
that is not `let mut` is an error. Free functions, by contrast, require the
caller to write the mode at the call, as chapter 8 shows. That asymmetry is
simply how the language is today; whether method calls should carry the mode
too is an open design question, tracked as RUE-2067.

## Associated functions

A function inside a struct that does *not* take `self` is an associated
function. It is called on the type, and the conventional use is a constructor
named `new`:

```rue run
const std = @import("std");

struct Point {
    x: i32,
    y: i32,

    fn new(x: i32, y: i32) -> Self {
        Self { x: x, y: y }
    }

    fn origin() -> Self {
        Self { x: 0, y: 0 }
    }

    fn manhattan(borrow self) -> i32 {
        let ax = if self.x < 0 { -self.x } else { self.x };
        let ay = if self.y < 0 { -self.y } else { self.y };
        ax + ay
    }
}

fn main() -> i32 {
    let p = Point.new(-3, 4);
    let o = Point.origin();
    println(@to_string(p.manhattan()) + " " + @to_string(o.manhattan()));
    0
}
```

```text
7 0
```

Chaining works for methods that take `self` by value and return a new
`Self`. A `borrow self` or `inout self` method needs a place to borrow from,
so it cannot be called directly on a call result: `Point.new(1, 2).manhattan()`
is rejected. Bind the value with `let` first, as `main` does above, and call
the method on the binding.

## Structs in functions

Passing a struct to a function is where ownership starts to matter. This works,
and you have seen the `borrow` keyword on `self` already:

```rue run
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn distance_squared(borrow a: Point, borrow b: Point) -> i32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dx * dx + dy * dy
}

fn main() -> i32 {
    let origin = Point { x: 0, y: 0 };
    let target = Point { x: 3, y: 4 };
    println(@to_string(distance_squared(borrow origin, borrow target)));
    println("origin is still here: " + @to_string(origin.x));
    0
}
```

```text
25
origin is still here: 0
```

Both the parameter and the argument say `borrow`: the function reads the
points without taking them. If you drop the keyword and pass a struct by
value, the value *moves* into the function and the caller cannot use it
afterwards. Chapter 8 is all about this. First, the other kind of user-defined
type.
