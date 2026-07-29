+++
title = "Compile-Time Expressions"
weight = 14
template = "spec/page.html"
+++

# Compile-Time Expressions

{{ rule(id="4.14:1", cat="normative") }}

A compile-time expression is an expression marked with the `comptime` keyword that **MUST** be fully evaluated at compile time.

{{ rule(id="4.14:2", cat="normative") }}

```ebnf
comptime_expr = "comptime" "{" block "}" ;
block         = { statement } [ expression ] ;
```

The block inside a comptime expression is evaluated during compilation using
the ordinary block-expression value rules (4.5): a tail expression supplies the
block's value, and a block without a tail expression evaluates to `()`. The
comptime expression evaluates to that compile-time value. The following
operations are supported within comptime blocks:

- Integer literals
- Boolean literals (`true`, `false`)
- Arithmetic operators (`+`, `-`, `*`, `/`, `%`) and unary negation (`-`)
- Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Logical operators (`&&`, `||`, `!`)
- Bitwise operators (`&`, `|`, `^`, `<<`, `>>`, `~`)
- `let` bindings of comptime values
- References to file-level constants and to `comptime` parameters in scope
- Fully-comptime calls (4.14:28): a call to a function whose parameters are all
  declared `comptime`, applied to comptime-evaluable arguments

Evaluation follows runtime semantics exactly: arithmetic is checked at the operands' type (Chapter 8.1), the full value range of every integer type is supported (including negative results and `u64` values above `i64::MAX`), shift amounts are masked modulo the bit width and shift results truncate to the operand width (4.3a:10), and `&&`/`||` short-circuit.

{{ rule(id="4.14:3", cat="normative") }}

A comptime expression can be used anywhere an expression is expected. The result of the comptime evaluation replaces the comptime block.

```rue
fn main() -> i32 {
    let x: i32 = comptime { 21 * 2 };
    x
}
```

## Comptime Restrictions

{{ rule(id="4.14:4", cat="legality-rule") }}

It is a compile-time error if an expression inside a comptime block cannot be evaluated at compile time. This includes:

- References to runtime variables
- Calls that are not fully-comptime calls (4.14:28): a call to a function with
  any non-`comptime` parameter is never comptime-evaluable, because that
  function's body runs at runtime, and a call to an all-`comptime` function is
  comptime-evaluable only when every argument is itself comptime-evaluable
- Operations that would panic at runtime: integer overflow at the operands' type (including in intermediate results), division by zero, and remainder by zero

```rue
fn main() -> i32 {
    let x = 10;
    comptime { x + 1 }  // ERROR: x cannot be known at compile time
}
```

A fully-comptime call is permitted, because every parameter is bound to a
compile-time value:

```rue
fn inc(comptime n: i32) -> i32 { n + 1 }

fn main() -> i32 {
    comptime { inc(4) }  // 5: fully-comptime call (4.14:28)
}
```

A call to `fn add(a: i32, b: i32)` would be rejected in the same position
because `add` has runtime parameters, and `inc(x)` would be rejected for a
runtime `x` because the argument is not comptime-evaluable.

## Comptime Parameters

{{ rule(id="4.14:5", cat="normative") }}

Function parameters can be marked with `comptime`, requiring the caller to provide a compile-time known value. The parameter's value is available as a compile-time constant within the function body.

```ebnf
parameter = [ "comptime" ] IDENT ":" type ;
```

Comptime parameters can have any type, including the special `type` type (see below).

```rue
fn multiply(comptime n: i32, value: i32) -> i32 {
    n * value
}

fn main() -> i32 {
    multiply(6, 7)  // n is known at compile time
}
```

Comptime parameters enable monomorphization: each unique combination of comptime arguments creates a specialized version of the function.

The keyword `type` is a comptime-only type whose values are types themselves. A parameter of type `type` must be marked `comptime`.

```rue
fn identity(comptime T: type, x: T) -> T {
    x
}

fn main() -> i32 {
    identity(i32, 42)
}
```

When a function has a `comptime T: type` parameter, occurrences of `T` in parameter types and return types are substituted with the concrete type at each call site.

{{ rule(id="4.14:16", cat="normative") }}

A type parameter may appear anywhere within a composite parameter or return type — as an array element type (`[T; N]`), a pointer pointee (`ptr const T`, `ptr mut T`), or a nesting of these (`[[T; 2]; 3]`) — and is substituted recursively at each call site, in both parameter and return position.

```rue
fn first(comptime T: type, a: [T; 3]) -> T {
    a[0]
}

fn pair(comptime T: type, x: T) -> [T; 2] {
    [x, x]
}

fn main() -> i32 {
    let xs = [10, 20, 30];
    first(i32, xs) + pair(i32, 6)[0]  // 16
}
```

{{ rule(id="4.14:6", cat="legality-rule") }}

It is a compile-time error to pass a runtime value to a comptime parameter.

```rue
fn double(comptime n: i32) -> i32 { n * 2 }

fn main() -> i32 {
    let x = 21;
    double(x)  // ERROR: comptime parameter requires a compile-time known value
}
```

Type values cannot exist at runtime. It is a compile-time error to attempt to store a type value in a runtime variable.

```rue
fn main() -> i32 {
    let t = comptime { i32 };  // ERROR: type values cannot exist at runtime
    0
}
```

{{ rule(id="4.14:17", cat="normative") }}

Within a specialized function body, an `if` expression whose condition can be evaluated at compile time (because it references comptime parameters in scope) selects its branch at compile time: only the taken branch is analyzed and compiled. This permits comptime-recursive functions, whose recursive call sits in a branch that is not taken once the recursion reaches its base case.

```rue
fn fact(comptime n: i32) -> i32 {
    if n <= 1 { 1 } else { n * fact(n - 1) }
}

fn main() -> i32 {
    fact(5)  // 120: fact(5) .. fact(1) are specialized; fact(1) takes the
             // then-branch, so the recursion terminates
}
```

{{ rule(id="4.14:18", cat="legality-rule") }}

It is a compile-time error when specialization exceeds the implementation's maximum nesting depth (at least 64 levels), as happens when a comptime-recursive function never reaches a compile-time-known base case or a generic function recursively instantiates itself with new types. Implementations **MUST** diagnose this instead of failing to terminate.

```rue
fn runaway(comptime n: i32) -> i32 {
    runaway(n + 1)  // ERROR: exceeds the maximum specialization depth
}
```

{{ rule(id="4.14:19", cat="normative") }}

Within a specialized function body, a `match` expression whose scrutinee can be evaluated at compile time likewise selects its arm at compile time: only the body of the first arm whose pattern matches the comptime value is analyzed and compiled. Comptime recursion may therefore equivalently be written with `match`. The pattern set itself must still be exhaustive (4.7:9) — exhaustiveness is a property of the patterns, which are checked even though unselected arm bodies are not.

```rue
fn fact(comptime n: i32) -> i32 {
    match n {
        0 => 1,
        1 => 1,
        _ => n * fact(n - 1),  // not analyzed once n reaches 1
    }
}

fn main() -> i32 {
    fact(5)  // 120: the recursion terminates exactly as with `if`
}
```

## Anonymous Struct Types

{{ rule(id="4.14:7", cat="normative") }}

A comptime function that returns `type` can construct an anonymous struct type using the following syntax:

```ebnf
anon_struct_type = "struct" "{" struct_field { "," struct_field } "}" ;
struct_field = IDENT ":" type ;
```

```rue
fn Point() -> type {
    struct { x: i32, y: i32 }
}

fn main() -> i32 {
    let P = Point();
    let p: P = P { x: 10, y: 32 };
    p.x + p.y
}
```

Anonymous structs can be parameterized using comptime type parameters:

```rue
fn Pair(comptime T: type) -> type {
    struct { first: T, second: T }
}

fn main() -> i32 {
    let IntPair = Pair(i32);
    let p: IntPair = IntPair { first: 20, second: 22 };
    p.first + p.second
}
```

{{ rule(id="4.14:8", cat="normative") }}

Each anonymous struct declaration expression denotes a producer-nominal type.
Its identity is the selected declaration expression under its static enclosing
comptime specialization. Declared comptime arguments and enclosing
specializations distinguish anonymous type specializations. Repeated evaluation of the same
declaration expression under the same canonical specialization denotes the
same type. A different declaration expression or specialization denotes a
different type, regardless
of equal fields, method signatures, or method bodies.

```rue
fn make_point1() -> type { struct { x: i32, y: i32 } }
fn make_point2() -> type { struct { x: i32, y: i32 } }

fn main() -> i32 {
    let P1 = make_point1();
    let P2 = make_point2();
    let p1: P1 = P1 { x: 10, y: 20 };
    let p2: P2 = p1;  // ERROR: P1 and P2 have different producers
    p2.x + p2.y
}
```

Anonymous structs produced by different declaration expressions or
specializations are different types and are not assignable to each other,
including when their fields are equal.

{{ rule(id="4.14:9", cat="legality-rule") }}

It is a compile-time error to define an anonymous struct type with no fields and no methods.

```rue
fn empty() -> type {
    struct { }  // ERROR: empty struct
}
```

## Anonymous Struct Methods

{{ rule(id="4.14:10", cat="normative") }}

An anonymous struct type can include method definitions using the following syntax:

```ebnf
anon_struct_type = "struct" "{" [ struct_field { "," struct_field } ] [ method_def { method_def } ] "}" ;
method_def = "fn" IDENT "(" [ param { "," param } ] ")" [ "->" type ] block ;
```

Methods defined inside an anonymous struct type become methods on that struct type:

```rue
fn Counter() -> type {
    struct {
        value: i32,

        fn increment(self) -> Self {
            Self { value: self.value + 1 }
        }

        fn get(self) -> i32 {
            self.value
        }
    }
}

fn main() -> i32 {
    let C = Counter();
    let c: C = C { value: 0 };
    let c2 = c.increment();
    c2.get()
}
```

{{ rule(id="4.14:11", cat="normative") }}

Inside an anonymous struct's method definitions, `Self` refers to the anonymous struct type being defined. `Self` can be used as a type annotation, in struct literal expressions, and as a return type.

```rue
fn Pair(comptime T: type) -> type {
    struct {
        first: T,
        second: T,

        fn swap(self) -> Self {
            Self { first: self.second, second: self.first }
        }
    }
}
```

{{ rule(id="4.14:12", cat="normative") }}

Methods inside anonymous structs can access comptime parameters from the enclosing function:

```rue
fn Array(comptime T: type, comptime N: i32) -> type {
    struct {
        len: i32,

        fn capacity(self) -> i32 {
            N  // Captured from enclosing comptime context
        }
    }
}
```

{{ rule(id="4.14:13", cat="normative") }}

Functions defined without a `self` parameter are associated functions, called using the `Type.function()` syntax:

```rue
fn Point() -> type {
    struct {
        x: i32,
        y: i32,

        fn origin() -> Self {
            Self { x: 0, y: 0 }
        }
    }
}

fn main() -> i32 {
    let P = Point();
    let p = P.origin();
    p.x
}
```

{{ rule(id="4.14:14", cat="legality-rule") }}

It is a compile-time error to define two methods with the same name in an anonymous struct type.

{{ rule(id="4.14:15", cat="normative") }}

Method definitions are content of the producer-nominal anonymous struct type
selected by their enclosing declaration expression. Fields, method names,
signatures, declaration order, and method bodies do not make two different
anonymous struct declaration expressions the same type. `Self` in each method
denotes that enclosing producer-nominal type.

```rue
fn A() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x } }
}

fn B() -> type {
    // Different from A(): B's declaration expression is a distinct producer.
    struct { x: i32, fn get(self) -> i32 { self.x } }
}

fn C() -> type {
    // Also different from A() and B(), independently of this signature change.
    struct { x: i32, fn get(self) -> i64 { @intCast(self.x) } }
}
```

{{ rule(id="4.14:20", cat="normative") }}

A comptime function that returns `type` can construct an anonymous enum (sum) type using the following syntax:

```ebnf
anon_enum_type = "enum" "{" [ enum_variant { "," enum_variant } ] "}" ;
enum_variant = IDENT [ "(" type { "," type } ")" ] ;
```

Anonymous enums are the sum-type analog of anonymous struct types (rule 4.14:7) and may be parameterized by comptime type parameters, which makes generic sum types such as `Option` and `Result` expressible as ordinary library functions rather than compiler builtins:

```rue
fn Option(comptime T: type) -> type {
    enum { Some(T), None }
}

fn main() -> i32 {
    let O = Option(i32);
    let x: O = O.Some(5);
    match x { O.Some(n) => n, O.None => 0 }
}
```

Each instantiation is monomorphized: `Option(i32)` and `Option(bool)` are distinct types with independent tagged-union layouts (the payload types differ).

{{ rule(id="4.14:21", cat="normative") }}

Each anonymous enum declaration expression denotes a producer-nominal type.
Its identity is the selected declaration expression under its static enclosing
comptime specialization. Declared comptime arguments and enclosing
specializations distinguish anonymous type specializations. Repeated evaluation of the same
declaration expression under the same canonical specialization denotes the
same type. A different declaration expression or specialization denotes a
different type, regardless
of equal variant names or payload types.

```rue
fn Option(comptime T: type) -> type { enum { Some(T), None } }

fn main() -> i32 {
    let A = Option(i32);
    let B = Option(i32);
    let x: A = A.Some(10);
    let y: B = x;  // OK: A and B select the same producer and specialization
    match y { B.Some(n) => n, B.None => 0 }
}
```

## Generic Types (Comptime Type Functions)

The rules in this section describe the generics mechanism of Rue. A generic
type is not a distinct language construct: it is an ordinary function whose
comptime parameters and `type` return make it a *type constructor*. This is how
`Option`, `Result`, and array-buffer types are expressed as library code rather
than compiler builtins.

{{ rule(id="4.14:22", cat="normative") }}

A comptime function whose declared return type is `type` is a *type
constructor* (equivalently, a *generic type*). Its body evaluates at compile
time to any comptime type value. When evaluation selects an anonymous struct
declaration expression or anonymous enum declaration expression, that
expression denotes the producer-nominal type defined by rules 4.14:8 and
4.14:21. When evaluation returns an existing type value, it preserves that
type's identity. Calling a type constructor is *type-function application*;
the call is evaluated at compile time and reduces to that concrete type.
Because application is comptime evaluation, every argument must be
compile-time known (rule 4.14:6), and each `type`-typed argument must be
supplied by a `comptime` parameter or another type value.

In *value position*, the reduced type is an ordinary compile-time type value:
it may be bound with `let` and then used as the path of a struct-literal
expression (`P { … }`), a method call, or an associated-function call
(`P.origin()`), exactly as in rules 4.14:7 through 4.14:13.

```rue
fn Option(comptime T: type) -> type { enum { Some(T), None } }

fn main() -> i32 {
    let O = Option(i32);        // type-function application in value position
    let x: O = O.Some(42);
    match x { O.Some(n) => n, O.None => 0 }
}
```

A type constructor may forward an existing type value without minting another
type:

```rue
fn Id(comptime T: type) -> type { T }
fn Pair(comptime T: type) -> type { struct { first: T, second: T } }

fn main() -> i32 {
    let P = Pair(i32);
    let Q = Id(P);
    let p: P = P { first: 20, second: 22 };
    let q: Q = p;  // OK: Q preserves P's identity
    q.first + q.second
}
```

{{ rule(id="4.14:23", cat="normative") }}

A type-constructor call may appear directly wherever a type is expected — in a
`let` type annotation, a function parameter type, a function return type, a
struct field type, an array element type (`[F(i32); N]`), or a pointer pointee
— and may be nested within composite types (rule 4.14:16). The call is
evaluated at compile time and the resulting concrete type is substituted in
that position.

```rue
fn Pair(comptime T: type) -> type { struct { first: T, second: T } }

fn mk() -> Pair(i32) {                 // application in return position
    let P = Pair(i32);
    P { first: 40, second: 2 }
}

fn sum(p: Pair(i32)) -> i32 {          // application in parameter position
    p.first + p.second
}

fn main() -> i32 {
    let p: Pair(i32) = mk();           // application in annotation position
    sum(p)
}
```

A type-constructor call may also be used directly as a path head, with explicit
(compile-time-known) arguments. It may head a struct literal
(`Pair(i32) { first: 1, second: 2 }`), an associated-function or enum-variant
call (`Result(i32, i32).Ok(v)`, `Vec(i32).new()`), and a match pattern
(`match r { Result(i32, i32).Ok(v) => … }`). The type constructor itself may be
reached through a module path, so a module-qualified head is admitted in each of
these positions, including the pattern head (`std.result.Result(i32, i32).Ok(v)`).
Each is evaluated exactly as if the call had been bound to a name first — with
`let P = F(args);` and then `P { … }` or `P.NAME`, which remains an equivalent
spelling — so the inline form is pure surface sugar and adds no new typing rule.
Eliding the arguments (`Option(_).Some(5)`) is not accepted: the arguments must
be written explicitly.

{{ rule(id="4.14:23a", cat="legality-rule") }}

An anonymous struct or enum declaration expression may not appear directly or
nested within a type annotation. This restriction applies to `let`, parameter,
return, field, array-element, and pointer-pointee annotations. A type
constructor call remains permitted in those positions provided its argument
expressions do not themselves contain an anonymous declaration expression.
This containment test is syntactic: it examines the spelling of the annotation
and its argument expressions, not the type values to which those expressions
evaluate. Anonymous declaration expressions remain permitted as comptime
values and as type-constructor results; a program that needs to use one in an
annotation first binds the type value or names it through a type constructor.
Value-position and path-head uses described by rules 4.14:22 and 4.14:23 are
not type annotations; they remain governed by the path-head grammar.

```rue
fn Pair(comptime T: type) -> type { struct { first: T, second: T } }

fn main() -> i32 {
    // ERROR: an anonymous struct type cannot appear in a type annotation.
    let p: struct { x: i32, y: i32 } = Pair(i32) { first: 1, second: 2 };
    p.first
}
```

{{ rule(id="4.14:24", cat="normative") }}

The comptime parameters of a type constructor — both `type` parameters and
ordinary `comptime` value parameters — are in scope throughout its entire body,
including the signatures and bodies of the methods and associated functions
(rules 4.14:10, 4.14:13) of the anonymous type it returns (extending rule
4.14:12, which captures a value parameter, to `type` parameters). A `type`
parameter `T` may be used anywhere a type is expected — as a field type, a
method parameter or return type, or a local `let` annotation — and may be
passed as an argument to another type constructor (for example `Option(T)`),
nesting generic types.

```rue
fn Option(comptime T: type) -> type { enum { Some(T), None } }

fn Wrap(comptime T: type) -> type {
    struct {
        inner: Option(T),               // T passed to another constructor
        fn get_or(self, d: T) -> T {    // T names the parameter/return type
            let O = Option(T);          // T in scope in the method body
            match self.inner { O.Some(v) => v, O.None => d }
        }
    }
}

fn main() -> i32 {
    let W = Wrap(i32);
    let O = Option(i32);
    let w: W = W { inner: O.Some(7) };
    w.get_or(0)
}
```

{{ rule(id="4.14:25", cat="normative") }}

Type-function application monomorphizes each canonical specialization
independently. When evaluation selects an anonymous struct or enum declaration
expression, that expression under the application's static enclosing comptime
specialization determines the resulting type identity. Different canonical
arguments or enclosing specializations select distinct specializations; equal
canonical specializations select the same type wherever evaluated. A function
that returns an existing type value, rather than selecting an anonymous
declaration expression, preserves the returned type's identity. Aliases also
preserve identity. Distinct producer-nominal specializations do not converge
merely because their contents are equal. Recursive instantiation that selects a
new specialization at each step remains subject to the specialization-depth
limit in 4.14:18.

```rue
fn Pair(comptime T: type) -> type { struct { first: T, second: T } }

fn produce() -> Pair(i32) {
    let P = Pair(i32);
    P { first: 10, second: 5 }
}

fn consume(p: Pair(i32)) -> i32 {  // same producer and specialization
    p.first + p.second
}

fn main() -> i32 {
    consume(produce())  // 15
}
```

## The Comptime-Evaluable Set

The rules for comptime branch selection (4.14:17, 4.14:19), comptime
parameters (4.14:5, 4.14:6), and type-function application (4.14:22) all turn
on whether a given expression is *comptime-evaluable* — known at compile time.
This section defines that set inductively: rule 4.14:26 gives the base cases,
rules 4.14:27 and 4.14:28 give the closure (inductive) cases, and rule 4.14:29
closes the set — nothing outside these clauses is comptime-evaluable.

{{ rule(id="4.14:26", cat="normative") }}

An expression is **comptime-evaluable** in a given scope in each of the
following base cases:

- an integer literal, a boolean literal (`true`, `false`), or the unit value
  (`()`);
- a reference to a `const` item (Chapter 6), whose initializer is itself
  comptime-evaluable — every `const` initializer is required to be
  comptime-evaluable, so every `const` reference qualifies. This includes a
  module-qualified member-access path that names a named type re-exported
  through an import chain (`std.strbuf.StrBuf`), which denotes the same
  comptime-evaluable type a qualified type annotation of that spelling resolves;
- a reference to a `comptime` parameter in scope (4.14:5), including a
  `comptime T: type` parameter, whose bound value is fixed at each
  specialization.

A reference to a runtime binding is **not** comptime-evaluable: neither a
non-`comptime` `let` binding (Chapter 6) nor a non-`comptime` function
parameter is known at compile time. Using such a reference where a
comptime-evaluable expression is required is a compile-time error (4.14:6,
diagnostic `E1201`).

```rue
const K: i32 = 3;

fn dbl(comptime n: i32) -> i32 { n * 2 }

fn main() -> i32 {
    dbl(K)  // 6: `K` is a const reference, hence comptime-evaluable
}
```

{{ rule(id="4.14:27", cat="normative") }}

The comptime-evaluable set is **closed under the value-forming operators and
grouping constructs**: an expression built from one of the following is
comptime-evaluable whenever all of its operand expressions are
comptime-evaluable, and is otherwise not:

- arithmetic operators (`+`, `-`, `*`, `/`, `%`) and unary negation (`-`);
- comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`);
- logical operators (`&&`, `||`, `!`);
- bitwise operators (`&`, `|`, `^`, `<<`, `>>`, `~`);
- parenthesization `( e )`;
- a block `{ … }` — including a `comptime { … }` block (4.14:2) — whose `let`
  initializers and tail expression are comptime-evaluable.

Evaluation of these forms follows runtime semantics exactly, subject to the
comptime overflow and division restrictions of 4.14:4. If any operand is not
comptime-evaluable (for example, a runtime `let` reference), the whole
expression is not comptime-evaluable.

```rue
const K: i32 = 3;

fn t(comptime n: i32) -> i32 {
    if (n & 1) == 1 && n > 0 { 1 } else { 0 }  // comptime-if: operands are
                                               // the comptime param `n`
}

fn main() -> i32 {
    t(K + 1)  // arg is `const + literal`, hence comptime-evaluable  -> 0
}
```

{{ rule(id="4.14:28", cat="normative") }}

The comptime-evaluable set is **closed under fully-comptime calls**: a call
expression `f(a₁, …, aₙ)` is comptime-evaluable if and only if every parameter
of `f` is declared `comptime` and every argument `aᵢ` is comptime-evaluable. A
call to a function that has any non-`comptime` parameter is **not**
comptime-evaluable, even when its arguments are, because that function's body
runs at runtime. Type-function application (4.14:22) is the special case of
this rule whose result is a `type` value; a fully-comptime call reduces at
compile time by the same evaluation used for monomorphization identity
(4.14:25).

```rue
fn inc(comptime n: i32) -> i32 { n + 1 }
fn dbl(comptime n: i32) -> i32 { n * 2 }

fn main() -> i32 {
    dbl(inc(4))  // 10: `inc(4)` is a fully-comptime call, so it is a
                 // comptime-evaluable argument to `dbl`
}
```

{{ rule(id="4.14:29", cat="legality-rule") }}

The comptime-evaluable set is exactly the least set closed under rules
4.14:26 through 4.14:28; **no other expression is comptime-evaluable**. In
particular, the type-introspection intrinsics `@size_of` and `@align_of`
(§4.13) are *not* comptime-evaluable in the current language: although their
results are compile-time constants, an implementation is not required to fold
them into the comptime-evaluable set, and this one does not. Using such an
expression where comptime evaluation is required is a compile-time error — in a
`comptime` argument position (4.14:6) the diagnostic is `E1201`, and in a
`const` initializer (Chapter 6) it is `E0434`. A future revision may enlarge
the set; a program that relies on an expression *not* covered by rules
4.14:26–4.14:28 being comptime-evaluable is non-portable.

```rue
fn dbl(comptime n: i32) -> i32 { n * 2 }

fn main() -> i32 {
    dbl(@size_of(i32))  // ERROR (E1201): @size_of is not comptime-evaluable,
                        // so it cannot be a comptime argument
}
```
