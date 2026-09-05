+++
title = "Project: Modules and Tests"
weight = 15
template = "tutorial/page.html"
+++

# Project: Modules and Tests

The calculator from the previous chapter is one file with three concerns in
it: the error type, the stack machine, and the line reader. This chapter moves
the machine into its own module, exposes exactly what the main file needs, and
adds tests to both files.

## The machine module

Create a directory `rpn` and put the machine in `rpn/machine.rue`:

```rue file=rpn/machine.rue
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const Stack = std.arraybuf.ArrayBuf(i64);
const OptI64 = std.option.Option(i64);

pub enum EvalError {
    UnexpectedByte(u8),
    StackUnderflow,
    DivisionByZero,
    LeftoverValues(u64),
}

pub const EvalResult = std.result.Result(i64, EvalError);
const StepResult = std.result.Result((), EvalError);

pub fn describe(e: EvalError) -> StrBuf {
    match e {
        EvalError.UnexpectedByte(b) => "unexpected character with byte value " + @to_string(b),
        EvalError.StackUnderflow => "not enough values on the stack",
        EvalError.DivisionByZero => "division by zero",
        EvalError.LeftoverValues(n) => @to_string(n) + " values left on the stack",
    }
}

pub struct Machine {
    stack: Stack,

    fn new() -> Self {
        Self { stack: Stack.new() }
    }

    fn push(inout self, value: i64) {
        self.stack.push(value);
    }

    fn pop(inout self) -> EvalResult {
        match self.stack.pop() {
            OptI64.Some(v) => EvalResult.Ok(v),
            OptI64.None => EvalResult.Err(EvalError.StackUnderflow),
        }
    }

    fn apply(inout self, op: u8) -> StepResult {
        let right = self.pop()?;
        let left = self.pop()?;
        let value = match op {
            b'+' => left + right,
            b'-' => left - right,
            b'*' => left * right,
            _ => {
                if right == 0 {
                    return StepResult.Err(EvalError.DivisionByZero);
                }
                left / right
            },
        };
        self.push(value);
        StepResult.Ok(())
    }

    fn finish(inout self) -> EvalResult {
        let value = self.pop()?;
        let leftover = self.stack.len();
        if leftover > 0 {
            return EvalResult.Err(EvalError.LeftoverValues(leftover));
        }
        EvalResult.Ok(value)
    }
}

test "apply adds the top two values" {
    let mut m = Machine.new();
    m.push(3);
    m.push(4);
    m.apply(b'+')?;
    let v = m.finish()?;
    @assert_eq(v, 7);
}

test "finish reports leftover values" {
    let mut m = Machine.new();
    m.push(1);
    m.push(2);
    match m.finish() {
        EvalResult.Ok(_) => @panic("expected an error"),
        EvalResult.Err(e) => @assert_eq(describe(e), "1 values left on the stack"),
    }
}

test "popping an empty machine underflows" {
    let mut m = Machine.new();
    match m.pop() {
        EvalResult.Ok(_) => @panic("expected an error"),
        EvalResult.Err(e) => @assert_eq(describe(e), "not enough values on the stack"),
    }
}
```

The code is the same as before with three changes.

**Visibility.** `EvalError`, `EvalResult`, `describe`, and `Machine` are
`pub`, because `main.rue` uses them. `StepResult`, `Stack`, and the aliases
are private: they are implementation details, and a reader of the public
surface does not need to see them. A `pub const` holding a type is how a
module exports an instantiation like `Result(i64, EvalError)` under a name.

**Tests live with the code.** Three `test` blocks exercise the machine
through its public methods. The first uses `?` on both `apply` and `finish`,
so a failure in either fails the test with the error value. The other two
check that errors come out as the right variant by matching and comparing the
description.

**No `main`.** A module does not need one. The root file provides it.

## The root file

Now `main.rue`, beside the `rpn` directory:

```rue run stdin="3 4 +\n1 +\n"
const std = @import("std");
const machine = @import("rpn/machine.rue");
const StrBuf = std.strbuf.StrBuf;
const OptLine = std.option.Option(StrBuf);
const EvalResult = machine.EvalResult;
const EvalError = machine.EvalError;

fn is_digit(b: u8) -> bool {
    b >= b'0' && b <= b'9'
}

fn is_operator(b: u8) -> bool {
    b == b'+' || b == b'-' || b == b'*' || b == b'/'
}

fn eval_line(line: StrBuf) -> EvalResult {
    let mut m = machine.Machine.new();
    let mut number: i64 = 0;
    let mut in_number = false;
    for b in line {
        if is_digit(b) {
            number = number * 10 + @intCast(b - b'0');
            in_number = true;
        } else {
            if in_number {
                m.push(number);
                number = 0;
                in_number = false;
            }
            if is_operator(b) {
                m.apply(b)?;
            } else if b != b' ' {
                return EvalResult.Err(EvalError.UnexpectedByte(b));
            }
        }
    }
    if in_number {
        m.push(number);
    }
    m.finish()
}

test "a whole line evaluates" {
    let v = eval_line("2 3 4 * +")?;
    @assert_eq(v, 14);
}

test "an unexpected character is reported" {
    match eval_line("7 x") {
        EvalResult.Ok(_) => @panic("expected an error"),
        EvalResult.Err(e) => @assert_eq(machine.describe(e), "unexpected character with byte value 120"),
    }
}

fn main() -> i32 {
    loop {
        let line = match @read_line() {
            OptLine.Some(l) => l,
            OptLine.None => break,
        };
        match eval_line(line) {
            EvalResult.Ok(v) => println(@to_string(v)),
            EvalResult.Err(e) => println("error: " + machine.describe(e)),
        }
    }
    0
}
```

```bash
printf '3 4 +\n1 +\n' | scripts/rue exec main.rue
```

```text
7
error: not enough values on the stack
```

The root imports the module, binds the two names it uses most to short
aliases, and otherwise reads as before. `machine.Machine.new()` and
`machine.describe(e)` say where they come from. Nothing in this file is
private to the module it uses, and nothing in the module is reachable that the
module did not mark `pub`.

`EvalError` is a `const` bound to `machine.EvalError`, and `eval_line` builds
a variant through that alias just as it would through the original name.

## Running the tests

```bash
"$RUE" test main.rue
```

```text
5 passed (0.0s)
```

`rue test` was given `main.rue`, followed its import to `rpn/machine.rue`, and
ran the tests from both files. Change the `@assert_eq` in "a whole line
evaluates" to expect `15` and run it again to see a failure report with the
left and right values and a `repro` command that runs only that test.

## What you built

Step back and look at the program as a whole. Every function that can fail
says so in its return type. Every call that mutates something has `inout` in
its declaration and, for free functions, at its call site. The only borrow in
the program is the reader's own attention: there are no lifetime annotations,
no reference types, and no place where a value is shared in a way the compiler
cannot see. The heap buffers behind every `StrBuf` and `ArrayBuf` are freed
exactly when their owners go out of scope. And the tests are ten lines away
from the code they test, compiled by the same compiler.

That is the shape Rue is trying to make natural.

## Try it yourself

- Move `eval_line` and its helpers into `rpn/eval.rue`, so `main.rue` is only
  the I/O loop. Which names need to become `pub`?
- Add a `Machine.depth()` query and a test for it.
- Make the machine reject a result that would not fit in an `i32`, using
  `std.math.checked_mul` and friends so that overflow becomes an `EvalError`
  instead of a trap.
