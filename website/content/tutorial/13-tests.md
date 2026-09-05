+++
title = "Tests"
weight = 13
template = "tutorial/page.html"
+++

# Tests

Tests are part of the language. A `test` declaration sits in the same file as
the code it checks, is compiled by the same compiler, and runs with the
`rue test` command. It is not part of the executable, so a program with tests
in it builds and runs exactly as before.

```rue check
const std = @import("std");

fn is_leap(year: i32) -> bool {
    if year % 400 == 0 {
        true
    } else if year % 100 == 0 {
        false
    } else {
        year % 4 == 0
    }
}

test "years divisible by 400 are leap years" {
    @assert(is_leap(2000), "2000 should be a leap year");
}

test "years divisible by 100 but not 400 are not" {
    @assert(!is_leap(1900), "1900 should not be a leap year");
}

test "ordinary years" {
    @assert_eq(is_leap(2024), true);
    @assert_eq(is_leap(2023), false);
}

fn main() -> i32 {
    if is_leap(2024) {
        println("2024 is a leap year");
    }
    0
}
```

Save it as `leap.rue` and run the tests. `rue test` takes the same root file a
normal compile does, and needs `RUE_STD_PATH` set as in chapter 2:

```bash
"$RUE" test leap.rue
```

```text
3 passed (0.0s)
```

Run the program as usual and the tests are simply not there:

```bash
scripts/rue exec leap.rue
```

```text
2024 is a leap year
```

## Writing a test

`test "name" { ... }` declares a test. The string is its name and must be
unique within the file. The body is an ordinary block that returns nothing; it
can call any function in the file, private ones included, and use anything the
file imports.

Inside the body, two intrinsics report failures:

- `@assert(condition, "message")` fails with the message if the condition is
  false.
- `@assert_eq(left, right)` fails if the two values differ, and reports both.

A failing test looks like this:

```rue check
fn double(x: i32) -> i32 {
    x * 2
}

test "double is not triple" {
    @assert_eq(double(2), 6);
}

fn main() -> i32 {
    0
}
```

```text
FAIL wrong.rue::double is not triple
  assert_eq: assertion failed: left == right  (wrong.rue:6:5)
  left:  4
  right: 6
  repro: rue test wrong.rue --filter 'wrong.rue::double is not triple' --seed ...
0 passed, 1 failed (0.0s)
```

The report names the test, the assertion, its file and line, and both values.
The `repro` line is a command that runs only that test with the same
settings, which is what you want to paste after fixing it.

## `?` in tests

A test body may use `?` on an `Option` or `Result`. A `None` or `Err` fails
the test and reports the value, so testing a function that returns `Result`
does not need a `match` for the happy path:

```rue check
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const OptI64 = std.option.Option(i64);
const IntResult = std.result.Result(i64, StrBuf);

fn parse(line: StrBuf) -> IntResult {
    match @parse_i64(line.clone()) {
        OptI64.Some(n) => IntResult.Ok(n),
        OptI64.None => IntResult.Err("not a number: " + line),
    }
}

test "parse accepts an integer" {
    let n = parse("42")?;
    @assert_eq(n, 42);
}

test "parse rejects text" {
    match parse("forty") {
        IntResult.Ok(_) => @panic("expected an error"),
        IntResult.Err(msg) => @assert_eq(msg, "not a number: forty"),
    }
}

fn main() -> i32 {
    0
}
```

## How tests run

`rue test` compiles the root file and every module it imports, collects the
tests it finds in all of them, and runs each one in its own process. That
isolation means a trap in one test (an overflow, an out-of-bounds index, a
`@panic`) fails that test and no other. Tests run in a shuffled order under a
seed that the runner reports, so an accidental dependency between two tests
shows up as a failure you can reproduce.

Useful flags:

- `--filter <text>` runs only tests whose id contains the text.
- `--list` prints the tests without running them.
- `--format json` emits a machine-readable event stream, for tools.
- `--timeout-ms <n>` bounds each test's running time.

Tests only run for files the root imports. A test file nothing imports is
silently ignored, so keep tests beside the code they test, or import a
dedicated test module from the root.

That is the whole language surface this tutorial covers. The last three
chapters put it to work.
