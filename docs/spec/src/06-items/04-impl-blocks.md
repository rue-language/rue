+++
title = "Impl Blocks"
weight = 4
template = "spec/page.html"
+++

# Impl Blocks

{{ rule(id="6.4:1", cat="normative") }}

An impl block associates methods and functions with a struct type.

{{ rule(id="6.4:2", cat="syntax") }}

```ebnf
impl_block = "impl" IDENT "{" { method_def } "}" ;
method_def = "fn" IDENT "(" [ method_params ] ")" [ "->" type ] block ;
method_params = method_param { "," method_param } [ "," ] ;
method_param = "self" | ( IDENT ":" type ) ;
```

## Methods

{{ rule(id="6.4:3", cat="normative") }}

A method is a function defined inside an impl block that takes `self` as its first parameter.

{{ rule(id="6.4:4", cat="normative") }}

The `self` parameter represents the receiver value and has the type of the impl block's target struct.

{{ rule(id="6.4:5", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

impl Point {
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
struct Point { x: i32, y: i32 }

impl Point {
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
struct Counter { value: i32 }

impl Counter {
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

A function in an impl block that does not take `self` as its first parameter is an associated function.

{{ rule(id="6.4:13", cat="normative") }}

Associated functions are called using path notation: `Type::function(args)`.

{{ rule(id="6.4:14", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

impl Point {
    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

fn main() -> i32 {
    let p = Point::origin();
    p.x  // Returns 0
}
```

## Multiple Impl Blocks

{{ rule(id="6.4:15", cat="normative") }}

Multiple impl blocks for the same struct type are allowed.

{{ rule(id="6.4:16", cat="legality-rule") }}

Method names **MUST** be unique across all impl blocks for a given struct type.

{{ rule(id="6.4:17", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

impl Point {
    fn get_x(self) -> i32 { self.x }
}

impl Point {
    fn get_y(self) -> i32 { self.y }
}

fn main() -> i32 {
    let p = Point { x: 42, y: 10 };
    p.get_x()  // Returns 42
}
```

## Impl Block Ordering

{{ rule(id="6.4:18", cat="informative") }}

An impl block may appear before or after the struct definition it implements.
Forward references are resolved during semantic analysis.

## Error Conditions

{{ rule(id="6.4:19", cat="legality-rule") }}

An impl block for an undefined struct type is a compile-time error.

{{ rule(id="6.4:20", cat="legality-rule") }}

Calling a method on a non-struct type is a compile-time error.

{{ rule(id="6.4:21", cat="legality-rule") }}

Calling an undefined method is a compile-time error.

{{ rule(id="6.4:22", cat="legality-rule") }}

Calling an associated function with method call syntax (receiver.function()) is a compile-time error.

{{ rule(id="6.4:23", cat="legality-rule") }}

Calling a method with associated function syntax (Type::method()) is a compile-time error.
