+++
title = "Project: A Calculator"
weight = 14
template = "tutorial/page.html"
+++

# Project: A Calculator

Time to write a real program. Over this chapter and the next you will build a
small calculator that reads expressions from standard input, one per line,
evaluates them, and prints the result or an error. It uses nearly everything
from the previous chapters: structs with methods, enums with payloads,
`Result` and `?`, `ArrayBuf`, byte-level string handling, `borrow` and
`inout`, and a read-until-end-of-input loop.

The calculator uses *reverse Polish notation*: operators come after their
operands, so `3 4 +` means 3 + 4, and `2 3 4 * +` means 2 + (3 × 4). RPN needs
no parentheses and no precedence rules, which keeps the evaluator to one
pass over the input with a stack of numbers.

## The whole program

Here is the finished single-file version. Save it as `rpn.rue`; the sections
below walk through it piece by piece.

```rue run stdin="3 4 +\n2 3 4 * +\n10 2 /\n1 +\n5 0 /\n1 2\n7 x\n"
const std = @import("std");
const StrBuf = std.strbuf.StrBuf;
const Stack = std.arraybuf.ArrayBuf(i64);
const OptI64 = std.option.Option(i64);
const OptLine = std.option.Option(StrBuf);

enum EvalError {
    UnexpectedByte(u8),
    StackUnderflow,
    DivisionByZero,
    LeftoverValues(u64),
}

const EvalResult = std.result.Result(i64, EvalError);
const StepResult = std.result.Result((), EvalError);

fn describe(e: EvalError) -> StrBuf {
    match e {
        EvalError.UnexpectedByte(b) => "unexpected character with byte value " + @to_string(b),
        EvalError.StackUnderflow => "not enough values on the stack",
        EvalError.DivisionByZero => "division by zero",
        EvalError.LeftoverValues(n) => @to_string(n) + " values left on the stack",
    }
}

struct Machine {
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
        let value = if op == 43 {
            left + right
        } else if op == 45 {
            left - right
        } else if op == 42 {
            left * right
        } else {
            if right == 0 {
                return StepResult.Err(EvalError.DivisionByZero);
            }
            left / right
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

fn is_digit(b: u8) -> bool {
    b >= 48 && b <= 57
}

fn is_operator(b: u8) -> bool {
    b == 43 || b == 45 || b == 42 || b == 47
}

fn eval_line(line: StrBuf) -> EvalResult {
    let mut machine = Machine.new();
    let mut number: i64 = 0;
    let mut in_number = false;
    for b in line {
        if is_digit(b) {
            number = number * 10 + @intCast(b - 48);
            in_number = true;
        } else {
            if in_number {
                machine.push(number);
                number = 0;
                in_number = false;
            }
            if is_operator(b) {
                machine.apply(b)?;
            } else if b != 32 {
                return EvalResult.Err(EvalError.UnexpectedByte(b));
            }
        }
    }
    if in_number {
        machine.push(number);
    }
    machine.finish()
}

fn main() -> i32 {
    loop {
        let line = match @read_line() {
            OptLine.Some(l) => l,
            OptLine.None => break,
        };
        match eval_line(line) {
            EvalResult.Ok(v) => println(@to_string(v)),
            EvalResult.Err(e) => println("error: " + describe(e)),
        }
    }
    0
}
```

Feed it some lines:

```bash
printf '3 4 +\n2 3 4 * +\n10 2 /\n1 +\n5 0 /\n1 2\n7 x\n' | scripts/rue exec rpn.rue
```

```text
7
14
5
error: not enough values on the stack
error: division by zero
error: 1 values left on the stack
error: unexpected character with byte value 120
```

Now let's look at how it is put together.

## Errors first

The program starts by deciding what can go wrong:

```rue skip
enum EvalError {
    UnexpectedByte(u8),
    StackUnderflow,
    DivisionByZero,
    LeftoverValues(u64),
}

const EvalResult = std.result.Result(i64, EvalError);
const StepResult = std.result.Result((), EvalError);
```

Every failure the evaluator can hit is a variant, and the two that have
something to report carry it as a payload. Two `Result` instantiations cover
the two shapes of function in the program: ones that produce a number and ones
that only succeed or fail, whose success value is the unit `()`.

`describe` turns an error into a message. It is the only place that knows the
wording, and the `match` inside it is exhaustive, so adding a variant later
means the compiler will point here.

## The machine

`Machine` wraps an `ArrayBuf(i64)` and gives it the vocabulary of a stack
calculator:

```rue skip
fn pop(inout self) -> EvalResult {
    match self.stack.pop() {
        OptI64.Some(v) => EvalResult.Ok(v),
        OptI64.None => EvalResult.Err(EvalError.StackUnderflow),
    }
}
```

`ArrayBuf.pop` returns an `Option`, because an empty buffer is not an error
from the buffer's point of view. For the calculator it is one, so `pop`
translates `None` into `StackUnderflow`. From here on, everything that pops
can use `?`.

`apply` is where that pays off:

```rue skip
fn apply(inout self, op: u8) -> StepResult {
    let right = self.pop()?;
    let left = self.pop()?;
    ...
```

Two pops, two possible early returns, each marked with `?`. The operator is a
byte: `43` is `+`, `45` is `-`, `42` is `*`, and `47` is `/`. Only division
has an extra failure mode. Notice that `apply` and `finish` both take
`inout self`: they change the stack, and their callers must hold a `let mut`
machine.

## Reading the line

`eval_line` walks the bytes of the line once:

```rue skip
for b in line {
    if is_digit(b) {
        number = number * 10 + @intCast(b - 48);
        in_number = true;
    } else {
        ...
```

A digit extends the number being read: `b - 48` converts the ASCII digit to
its value as a `u8`, and `@intCast` widens it to the `i64` the arithmetic
needs. Anything else ends the current number, if there was one, and is then
either an operator to apply, a space to skip, or an unexpected byte to report.

The `line` parameter is taken by value. `main` reads a fresh `StrBuf` per
line and never needs it again, so moving it into `eval_line` is the natural
choice, and the buffer is freed when `eval_line` returns.

## The main loop

`main` is the read-until-end-of-input loop from chapter 10 with the work
delegated:

```rue skip
match eval_line(line) {
    EvalResult.Ok(v) => println(@to_string(v)),
    EvalResult.Err(e) => println("error: " + describe(e)),
}
```

Every error the evaluator can produce arrives here as a value, is described,
and is printed. The program never traps on bad input, and there is no
exception anywhere between the failing `pop` and this `match`; just `?` on the
lines that can fail and a `Result` in every signature on the way.

## Try it yourself

Some things to add before moving on:

- Negative numbers. A `-` directly before a digit should start a negative
  literal rather than act as subtraction. What does `describe` need to know?
- A `dup` command that pushes a copy of the top value, and `swap`. You will
  need to read words as well as digits.
- Print the stack after each line instead of only the final value.

The next chapter splits the program into modules and adds tests.
