+++
title = "Errors and Traps"
weight = 11
template = "tutorial/page.html"
+++

# Errors and Traps

Rue has two answers to "what if this goes wrong", and the language keeps them
firmly apart. An expected failure is a **value** the caller must handle. A bug
is a **trap** that stops the program on the spot. There are no exceptions.

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const OptI64 = std.option.Option(i64);

enum ParseError {
    Empty,
    NotANumber,
    Negative(i64),
}

const ParseResult = std.result.Result(i64, ParseError);

fn parse_age(line: StrBuf) -> ParseResult {
    if line.is_empty() {
        return ParseResult.Err(ParseError.Empty);
    }
    let n = match @parse_i64(line) {
        OptI64.Some(n) => n,
        OptI64.None => return ParseResult.Err(ParseError.NotANumber),
    };
    if n < 0 {
        return ParseResult.Err(ParseError.Negative(n));
    }
    ParseResult.Ok(n)
}

fn describe(e: ParseError) -> StrBuf {
    match e {
        ParseError.Empty => "empty input",
        ParseError.NotANumber => "not a number",
        ParseError.Negative(n) => @to_string(n) + " is negative",
    }
}

fn report(line: StrBuf) {
    match parse_age(line) {
        ParseResult.Ok(age) => println("age " + @to_string(age)),
        ParseResult.Err(e) => println("error: " + describe(e)),
    }
}

fn main() -> i32 {
    report("42");
    report("");
    report("forty");
    report("-3");
    0
}
```

```text
age 42
error: empty input
error: not a number
error: -3 is negative
```

## `Result`: a value or an error

`Option` says "maybe nothing". `Result(T, E)` says "either a `T` or an `E`
explaining why not". Like `Option`, it is an ordinary enum from the standard
library, with variants `Ok(T)` and `Err(E)`, and you name the instantiation
you need with `const`.

The error type is whatever you choose. An enum with one variant per failure,
as above, is the usual choice: the caller can `match` on the reason, the
compiler checks that every reason is handled, and variants can carry details.
A function like `describe` that turns the enum into text keeps the wording in
one place. Matching on the error is a second `match` inside the `Err` arm;
patterns cannot yet nest one enum's variant inside another's payload.

## `?` with `Result`

`?` works on `Result` exactly as it does on `Option`: inside a function that
returns a `Result` with the same error type, `expr?` unwraps `Ok` or returns
the `Err` immediately.

```rue run
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const OptI64 = std.option.Option(i64);

enum CalcError {
    NotANumber,
    DivisionByZero,
}

const IntResult = std.result.Result(i64, CalcError);

fn parse(line: StrBuf) -> IntResult {
    match @parse_i64(line) {
        OptI64.Some(n) => IntResult.Ok(n),
        OptI64.None => IntResult.Err(CalcError.NotANumber),
    }
}

fn divide(a: i64, b: i64) -> IntResult {
    if b == 0 {
        return IntResult.Err(CalcError.DivisionByZero);
    }
    IntResult.Ok(a / b)
}

fn compute(left: StrBuf, right: StrBuf) -> IntResult {
    let a = parse(left)?;
    let b = parse(right)?;
    divide(a, b)
}

fn show(left: StrBuf, right: StrBuf) {
    match compute(left, right) {
        IntResult.Ok(v) => println("= " + @to_string(v)),
        IntResult.Err(e) => match e {
            CalcError.NotANumber => println("error: not a number"),
            CalcError.DivisionByZero => println("error: division by zero"),
        },
    }
}

fn main() -> i32 {
    show("84", "2");
    show("84", "x");
    show("84", "0");
    0
}
```

```text
= 42
error: not a number
error: division by zero
```

Three things can fail in `compute`, and each failure is one `?` away from the
caller. Read the function and you can see every early exit.

## Failures you cannot ignore

A `Result` is not just a convention. Because it is an enum, the only way to
get the value out is to `match` on it or apply `?`. There is no method that
quietly hands you the `Ok` payload and crashes otherwise, so a call whose
failure you forgot to think about does not compile as if it always succeeds.

## Traps

The other kind of failure is a program error: something that should never
happen if the code is correct. Rue does not try to recover from these. It
stops the program immediately with a message on standard error and exit
status `101`. The built-in traps are:

- integer overflow in `+`, `-`, `*`, and `@intCast`;
- an array index out of bounds;
- division or remainder by zero;
- reading invalid UTF-8 through `chars()`;
- an explicit `@panic("message")`;
- a failed `@assert(condition, "message")`.

```rue run exit=101
const std = @import("std");

fn checked_ratio(a: i32, b: i32) -> i32 {
    @assert(b != 0, "checked_ratio needs a nonzero divisor");
    a / b
}

fn main() -> i32 {
    println(@to_string(checked_ratio(10, 2)));
    println(@to_string(checked_ratio(10, 0)));
    println("not reached");
    0
}
```

```text
5
```

The second call prints `panic: checked_ratio needs a nonzero divisor` to
standard error and the program ends; the last line never runs.

A trap runs no user code on the way out: no destructors, no handlers. This is
deliberate. A program that has detected its own bug is not in a state anyone
should trust, and the safest thing it can do is stop where the evidence is. If
a condition is something a correct program can encounter, make it a `Result`.

## Which one to use

Ask whether the caller could reasonably do something about it.

- Input that might be malformed, a file that might be missing, a lookup that
  might miss: `Option` or `Result`.
- An index you computed being out of range, arithmetic you believed could not
  overflow, an invariant you thought your code maintained: a trap. Prefer
  `@assert` with a message for invariants, so the failure names itself.

`std.math` offers `checked_add`, `checked_sub`, and `checked_mul`, which return
`Option` instead of trapping, and `@wrapping_add` and friends for the rare
algorithms that want modular arithmetic. Both make the choice explicit at the
call site, which is the Rue way.

Next: splitting a program across files and using the rest of the standard
library.
