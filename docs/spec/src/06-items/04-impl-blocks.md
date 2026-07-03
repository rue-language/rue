+++
title = "Methods"
weight = 4
template = "spec/page.html"
+++

# Methods

{{ rule(id="6.4:1", cat="normative") }}

Methods are functions defined inside a struct block that can be called on instances of that struct.

{{ rule(id="6.4:2", cat="syntax") }}

```ebnf
struct_def = [ directives ] [ "pub" ] "struct" IDENT "{" [ field_list ] [ method_list ] "}" ;
field_list = field_def { "," field_def } [ "," ] ;
method_list = method_def { method_def } ;
method_def = [ directives ] "fn" IDENT "(" [ method_params ] ")" [ "->" type ] block ;
method_params = method_param { "," method_param } [ "," ] ;
method_param = receiver | ( [ "inout" | "borrow" ] IDENT ":" type ) ;
receiver = [ "inout" | "borrow" ] "self" ;
```

## Method Definition

{{ rule(id="6.4:3", cat="normative") }}

A method is a function defined inside a struct block that takes `self` as its first parameter.

{{ rule(id="6.4:4", cat="normative") }}

The `self` parameter represents the receiver value and has the type of the enclosing struct.

{{ rule(id="6.4:5", cat="example") }}

```rue
struct Point {
    x: i32,
    y: i32,

    fn get_x(self) -> i32 {
        self.x
    }
}

fn main() -> i32 {
    let p = Point { x: 42, y: 10 };
    p.get_x()  // Returns 42
}
```

## Method Calls

{{ rule(id="6.4:6", cat="normative") }}

Methods are called using dot notation: `receiver.method(args)`.

{{ rule(id="6.4:7", cat="dynamic-semantics") }}

A method call `receiver.method(args)` is desugared to a function call with the receiver as the first argument.

{{ rule(id="6.4:8", cat="normative") }}

Methods **MAY** have additional parameters after `self`.

{{ rule(id="6.4:9", cat="example") }}

```rue
struct Point {
    x: i32,
    y: i32,

    fn add(self, dx: i32, dy: i32) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    let p2 = p.add(32, 0);
    p2.x  // Returns 42
}
```

## Method Chaining

{{ rule(id="6.4:10", cat="normative") }}

When a method returns the same struct type, method calls **MAY** be chained.

{{ rule(id="6.4:11", cat="example") }}

```rue
struct Counter {
    value: i32,

    fn inc(self) -> Counter {
        Counter { value: self.value + 1 }
    }
}

fn main() -> i32 {
    let c = Counter { value: 39 };
    c.inc().inc().inc().value  // Returns 42
}
```

## Associated Functions

{{ rule(id="6.4:12", cat="normative") }}

A function in a struct block that does not take `self` as its first parameter is an associated function.

{{ rule(id="6.4:13", cat="normative") }}

Associated functions are called using path notation: `Type::function(args)`.

{{ rule(id="6.4:14", cat="example") }}

```rue
struct Point {
    x: i32,
    y: i32,

    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

fn main() -> i32 {
    let p = Point::origin();
    p.x  // Returns 0
}
```

## The `Self` Type

{{ rule(id="6.4:18", cat="normative") }}

Within a struct block, the type keyword `Self` denotes the enclosing struct
type. It **MAY** be used wherever a type is expected — in a method's parameter
types, in its return type, and in `Self { ... }` struct-literal expressions in
the body — and is equivalent to writing the struct's name.

{{ rule(id="6.4:19", cat="example") }}

```rue
struct Point {
    x: i32,
    y: i32,

    fn origin() -> Self {
        Self { x: 0, y: 0 }
    }

    fn translate(self, other: Self) -> Self {
        Self { x: self.x + other.x, y: self.y + other.y }
    }
}

fn main() -> i32 {
    let p = Point::origin().translate(Point { x: 42, y: 0 });
    p.x  // Returns 42
}
```

## Multiple Methods

{{ rule(id="6.4:15", cat="normative") }}

A struct may have multiple methods defined in its block.

{{ rule(id="6.4:16", cat="legality-rule") }}

Method names **MUST** be unique within a struct definition.

{{ rule(id="6.4:17", cat="example") }}

```rue
struct Point {
    x: i32,
    y: i32,

    fn get_x(self) -> i32 { self.x }

    fn get_y(self) -> i32 { self.y }
}

fn main() -> i32 {
    let p = Point { x: 42, y: 10 };
    p.get_x()  // Returns 42
}
```

## Error Conditions

{{ rule(id="6.4:20", cat="legality-rule") }}

Calling a method on a non-struct type is a compile-time error.

{{ rule(id="6.4:21", cat="legality-rule") }}

Calling an undefined method is a compile-time error.

{{ rule(id="6.4:22", cat="legality-rule") }}

Calling an associated function with method call syntax (receiver.function()) is a compile-time error.

{{ rule(id="6.4:23", cat="legality-rule") }}

Calling a method with associated function syntax (Type::method()) is a compile-time error.

## Receiver Modes

{{ rule(id="6.4:24", cat="normative") }}

A receiver **MAY** be declared `borrow self` or `inout self`, mirroring the
`borrow`/`inout` parameter modes. A bare `self` receiver is passed by value
(copied if the struct is `Copy`, otherwise moved). A `borrow self` receiver
grants read-only access to the receiver without taking ownership. An `inout
self` receiver grants exclusive mutable access without taking ownership, and
mutations to `self` are observed by the caller after the call returns.

{{ rule(id="6.4:25", cat="normative") }}

The receiver mode is a property of the method signature; it is **NOT** written
at the call site. A call `receiver.method(args)` accesses the receiver in the
method's declared mode automatically (autoref): by value for `self`, by
immutable reference for `borrow self`, and by exclusive mutable reference for
`inout self`.

{{ rule(id="6.4:26", cat="legality-rule") }}

Calling an `inout self` method requires the receiver to be a mutable place
(for example a `let mut` binding, or a field or element reachable from one). A
call whose receiver is not mutable is a compile-time error.

{{ rule(id="6.4:27", cat="legality-rule") }}

A `borrow self` or `inout self` method call whose receiver is not a place (for
example the result of a function or method call) is a compile-time error: the
receiver has no caller-visible storage to reference.

{{ rule(id="6.4:28", cat="legality-rule") }}

The law of exclusivity extends to receivers: the access implied by a `borrow
self` or `inout self` receiver participates in exclusivity checking together
with the call's `borrow`/`inout` arguments. Passing the receiver's own place
as an `inout` (or conflicting `borrow`) argument to the same call is a
compile-time error. An argument that only reads the receiver (for example
`v.push(v.len())`) is permitted, because its read completes before the
receiver access begins.

{{ rule(id="6.4:29", cat="dynamic-semantics") }}

A `borrow self` or `inout self` method call does not consume the receiver: the
receiver remains usable after the call. In particular, a `borrow self` getter
**MAY** be called repeatedly, and an `inout self` mutator mutates the receiver
in place rather than requiring the caller to rebuild it.

{{ rule(id="6.4:30", cat="example") }}

```rue
struct Counter {
    n: i32,

    fn get(borrow self) -> i32 {
        self.n
    }

    fn bump(inout self) {
        self.n = self.n + 1;
    }
}

fn main() -> i32 {
    let mut c = Counter { n: 0 };
    c.bump();
    c.bump();
    let a = c.get();   // borrow does not consume `c`
    c.bump();
    a + c.get()        // 2 + 3 = 5
}
```
