+++
title = "Ownership and Access Modes"
weight = 8
template = "tutorial/page.html"
+++

# Ownership and Access Modes

This is the chapter where Rue is most different from languages you may know.
Every value has one owner. Passing a value gives it away, unless you say you
are only lending it, and lending comes in two flavors: read-only `borrow` and
read-write `inout`. All of it is checked at compile time, and for free
functions all of it is written at the call site.

```rue run
const std = @import("std");

struct Account {
    balance: i64,
}

fn deposit(inout account: Account, amount: i64) {
    account.balance += amount;
}

fn report(borrow account: Account) {
    println("balance: " + @to_string(account.balance));
}

fn close(account: Account) -> i64 {
    println("closing with " + @to_string(account.balance));
    account.balance
}

fn main() -> i32 {
    let mut acct = Account { balance: 100 };
    deposit(inout acct, 50);
    report(borrow acct);
    let final = close(acct);
    println("returned " + @to_string(final));
    0
}
```

```text
balance: 150
closing with 150
returned 150
```

Read `main` on its own. `deposit` changes the account, `report` only looks at
it, and `close` takes it away. You know all three facts from the call sites,
before reading any of the functions.

## Moves

Integers, booleans, and floats are copied whenever you use them. Structs are
not. Passing a struct by value, assigning it to another binding, or returning
it *moves* it: the destination owns the value now, and the source binding is
dead.

```rue compile-fail E0205
struct Point {
    x: i32,
    y: i32,
}

fn consume(p: Point) -> i32 {
    p.x + p.y
}

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let first = consume(p);
    let second = consume(p);
    first + second
}
```

```text
error: [E0205]: use of moved value 'p'
  = help: to use `p` after the move, pass it by borrow instead: `borrow p`
```

A move is not a copy that happens to invalidate the source. It is the whole
point: a value lives in one place, so there is exactly one owner responsible
for cleaning it up, and nobody can observe it changing behind their back.

## Copies

Some structs are just bundles of numbers, and copying them is harmless. Mark
those with `@copy`, and they behave like integers:

```rue run
const std = @import("std");

@copy
struct Point {
    x: i32,
    y: i32,
}

fn main() -> i32 {
    let p1 = Point { x: 1, y: 2 };
    let mut p2 = p1;
    p2.x = 100;
    println(@to_string(p1.x) + " " + @to_string(p2.x));
    0
}
```

```text
1 100
```

`@copy` is only allowed when every field is itself copyable. A struct that owns
a heap buffer cannot be `@copy`, because two owners of one buffer is exactly
the bug ownership exists to prevent:

```rue compile-fail E0403
const std = @import("std");

@copy
struct Named {
    name: std.strbuf.StrBuf,
}

fn main() -> i32 {
    0
}
```

```text
error: [E0403]: @copy struct 'Named' has field 'name' with non-Copy type 'StrBuf'
```

The default is move, and you opt in to copy. That is the opposite of C and Go,
and it means an accidental duplicate of something expensive or unique is a
compile error rather than a surprise.

## `borrow`: lend for reading

Most functions do not want to own their arguments; they want to look at them.
A `borrow` parameter gives the function read access for the duration of the
call, and the caller keeps the value:

```rue run
const std = @import("std");

struct Point {
    x: i32,
    y: i32,
}

fn sum(borrow p: Point) -> i32 {
    p.x + p.y
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    println(@to_string(sum(borrow p)));
    println(@to_string(sum(borrow p)));
    println("p.x is still " + @to_string(p.x));
    0
}
```

```text
30
30
p.x is still 10
```

The keyword appears twice: in the signature and at the call. Both are
required, so a reader of either side knows what is happening:

```rue compile-fail E0432
struct Point {
    x: i32,
    y: i32,
}

fn sum(borrow p: Point) -> i32 {
    p.x + p.y
}

fn main() -> i32 {
    let p = Point { x: 10, y: 20 };
    sum(p)
}
```

```text
error: [E0432]: argument to borrow parameter must use 'borrow' keyword
```

## `inout`: lend for writing

An `inout` parameter gives the function write access to the caller's value.
The caller declares the binding `let mut`, and again the keyword appears on
both sides:

```rue run
const std = @import("std");

fn double_all(inout arr: [i32; 3]) {
    let mut i: u64 = 0;
    while i < 3 {
        arr[i] *= 2;
        i += 1;
    }
}

fn main() -> i32 {
    let mut values = [10, 20, 30];
    double_all(inout values);
    println(@to_string(values[0]) + " " + @to_string(values[1]) + " " + @to_string(values[2]));
    0
}
```

```text
20 40 60
```

```rue compile-fail E0431
fn bump(inout x: i32) {
    x += 1;
}

fn main() -> i32 {
    let mut v = 1;
    bump(v);
    v
}
```

```text
error: [E0431]: argument to inout parameter must use 'inout' keyword
```

`inout` works on any type, including plain integers. It is Rue's answer to
"return multiple values" and "modify in place" alike.

## One writer or many readers

While a value is lent `inout`, nothing else may touch it, not even for reading.
While it is lent `borrow`, it may be lent `borrow` again but not `inout`. The
compiler enforces this rule within a single call:

```rue compile-fail E0430
fn observe(borrow old: i32, inout new: i32) -> i32 {
    old + new
}

fn main() -> i32 {
    let mut x = 1;
    observe(borrow x, inout x)
}
```

```text
error: [E0430]: cannot borrow 'x' while it is mutably borrowed (inout)
```

This is the same guarantee Rust's borrow checker provides, but Rue can check
it with far less machinery, because a loan never outlives the call it is
passed to. There are no references you can store, so there is no need for
lifetimes to describe how long they last. The cost is that you cannot keep a
borrow around; the benefit is that you never have to explain one to the
compiler.

## Destructors

A type that owns something outside itself, like heap memory or a file handle,
needs cleanup when its owner is done. Declare a destructor with `drop fn`
named after the struct. It runs automatically when the value goes out of
scope:

```rue run
const std = @import("std");

struct Guard {
    id: i32,
}

drop fn Guard(self) {
    println("dropping guard " + @to_string(self.id));
}

fn main() -> i32 {
    let _outer = Guard { id: 1 };
    {
        let _inner = Guard { id: 2 };
        println("inside the block");
    }
    println("after the block");
    0
}
```

```text
inside the block
dropping guard 2
after the block
dropping guard 1
```

Values are dropped in reverse order of declaration, at the end of the scope
that owns them, deterministically. Because a moved value has a new owner, it is
dropped wherever that owner ends, not where it was created. Passing a `Guard`
into a function by value means the function drops it.

To end a value early, use `@drop`. It consumes the value, so using it
afterwards is the same error as using any moved value:

```rue run
const std = @import("std");

struct Guard {
    id: i32,
}

drop fn Guard(self) {
    println("dropping guard " + @to_string(self.id));
}

fn main() -> i32 {
    let g = Guard { id: 7 };
    println("before drop");
    @drop(g);
    println("after drop");
    0
}
```

```text
before drop
dropping guard 7
after drop
```

The standard library's `StrBuf` and `ArrayBuf` are ordinary structs with
destructors like this one. That is how Rue frees heap memory without a garbage
collector: every allocation has one owner, and the owner's destructor releases
it, at a point you can predict by reading the code.

## Method receivers, revisited

Now the `self` modes from chapter 6 should make sense. `borrow self` is a
borrow parameter, `inout self` is an inout parameter, and plain `self` consumes
the value. The only difference from free functions is that a method call
`r.scale(2)` does not repeat the keyword; the receiver's mode is fixed by the
declaration, and the compiler still requires `let mut` for an `inout` receiver.

Next up: the collections these rules were designed around.
