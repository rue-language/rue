# The first Rue program, across five languages

*2026-07-03. A dogfooding reflection: we wrote the first real Rue program
(`examples/first/stats.rue`, RUE-226), then wrote the same program in Rust,
Swift, Zig, and Hylo to see how Rue **feels** by comparison — and what that
tells us to build next.*

The task: **read integers from standard input and print how many there were,
their sum, and the maximum** (printing `(no input)` for an empty stream).

It is deliberately tiny, but it touches everything a real program needs: input,
a growable collection or a fold, parsing, an optional (the max of an empty set),
string formatting, and output. The shortlist is chosen on purpose:

- **Rust** — the incumbent Rue is most often measured against.
- **Swift** — value semantics + optionals, a mainstream ergonomics benchmark.
- **Zig** — comptime generics and explicit allocation, Rue's closest cousin on
  the *metaprogramming* axis.
- **Hylo** — mutable value semantics, `let`/`inout`/`sink` parameter conventions,
  second-class references, **no lifetimes**. Rue's closest cousin on the
  *ownership* axis; arguably the language Rue is most trying to be.

> Note: only `rustc` is installed in our environment, so the Rust version below
> is compiled and run; the Swift, Zig, and Hylo versions are written from
> knowledge of each language. Hylo's standard I/O is still experimental, so its
> version is the most approximate — treat its *access-model feel*, not its exact
> stdlib calls, as the signal.

---

## Rust (verified)

```rust
use std::io::{self, Read};

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).expect("read stdin");
    let nums: Vec<i64> = input
        .split_whitespace()
        .map(|tok| tok.parse().expect("not an integer"))
        .collect();
    let sum: i64 = nums.iter().sum();
    let max = nums.iter().copied().max();
    println!("count: {}", nums.len());
    println!("sum: {}", sum);
    match max {
        Some(m) => println!("max: {}", m),
        None => println!("max: (no input)"),
    }
}
```

Reads to EOF naturally, parses with iterator combinators, `Option<i64>` for the
max, and prints any integer width with `{}`. No sentinel, no count, no ceremony.

## Swift

```swift
var nums: [Int] = []
while let line = readLine() {          // readLine() -> String?  (nil at EOF)
    if let x = Int(line) { nums.append(x) }   // Int(_) -> Int?  (nil on failure)
}
print("count: \(nums.count)")
print("sum: \(nums.reduce(0, +))")
if let m = nums.max() {                // .max() -> Int?
    print("max: \(m)")
} else {
    print("max: (no input)")
}
```

Every fallible step yields an optional; `if let` handles both EOF and a bad
parse in the same breath. This is the ergonomic target.

## Zig

```zig
const std = @import("std");

pub fn main() !void {
    const stdin = std.io.getStdIn().reader();
    const stdout = std.io.getStdOut().writer();
    var buf: [64]u8 = undefined;

    var count: usize = 0;
    var sum: i64 = 0;
    var max: ?i64 = null;

    while (try stdin.readUntilDelimiterOrEof(&buf, '\n')) |line| {  // ?[]u8, null at EOF
        const t = std.mem.trim(u8, line, " \r\t");
        if (t.len == 0) continue;
        const x = std.fmt.parseInt(i64, t, 10) catch continue;     // error union
        count += 1;
        sum += x;
        max = if (max) |m| @max(m, x) else x;
    }

    try stdout.print("count: {d}\n", .{count});
    try stdout.print("sum: {d}\n", .{sum});
    if (max) |m| try stdout.print("max: {d}\n", .{m})
    else try stdout.print("max: (no input)\n", .{});
}
```

More explicit (fixed buffer, `try`/`catch`), but EOF is an optional payload and
parse failure is a catchable error. Note the shared DNA with Rue: `comptime`
generics, `@`-prefixed builtins, trapping arithmetic.

## Hylo (approximate — experimental I/O)

```
public fun main() {
  var nums: Array<Int> = []
  while let line = read_line() {          // Optional<String>, None at EOF (assumed)
    if let x = Int(line) { nums.append(x) }   // append takes `inout self`
  }
  print("count: \(nums.count())")
  print("sum: \(nums.reduce(0, fun (_ a: Int, _ x: Int) -> Int { a + x }))")
  if let m = nums.max() { print("max: \(m)") }
  else { print("max: (no input)") }
}
```

`nums` is a **value**; `append` mutates it through an `inout self` receiver —
exactly Rue's `Vec.push(inout self)`. No `&`, no lifetimes, no ARC. This is the
model Rue is built on, and the felt experience of the mutation is identical.

## Rue (today)

```rue
fn main() -> i32 {
    let OptStr = @import("../std/option.rue").Option(String);  // no prelude yet
    let OptInt = @import("../std/option.rue").Option(i64);

    let mut count: i64 = 0;
    let mut sum: i64 = 0;
    let mut max: OptInt = OptInt::None;
    loop {
        let line: OptStr = @read_line();                       // None at EOF
        match line {
            OptStr::None => break,                             // read-until-EOF
            OptStr::Some(text) => {
                match @parse_i64(text) {                       // None on bad input
                    OptInt::Some(x) => {
                        count = count + 1;
                        sum = sum + x;
                        max = match max {
                            OptInt::None => OptInt::Some(x),
                            OptInt::Some(m) => if x > m { OptInt::Some(x) } else { OptInt::Some(m) },
                        };
                    },
                    OptInt::None => {},                        // skip, no trap
                }
            },
        }
    }

    println("count: " + @to_string(count));
    println("sum: " + @to_string(sum));
    match max {
        OptInt::Some(m) => println("max: " + @to_string(m)),
        OptInt::None => println("max: (no input)"),
    }
    @intCast(count)
}
```

The logic is the same, and — since error handling landed (RUE-6, ADR-0038) —
the EOF and parse-failure seams are gone: this is a natural read-until-EOF loop
with no count prefix and no traps. The remaining friction is the imported
`Option` (no prelude yet) and i64-only `@to_string`.

---

## How Rue feels, dimension by dimension

| | Rust | Swift | Zig | Hylo | **Rue (today)** |
|---|---|---|---|---|---|
| **EOF** | iterator / read-to-end | `readLine() -> String?` | `…OrEof -> ?[]u8` | Optional | `@read_line -> Option(String)`, `None` at EOF |
| **Parse failure** | `Result` + `?` | `Int() -> Int?` | error union + `catch` | Optional | `@parse_i64 -> Option(i64)`, `None` on bad input |
| **Optional type** | `Option<T>` (prelude) | `T?` (built in) | `?T` (built in) | `Optional` (std) | **`@import`ed** — no prelude |
| **Error propagation** | `?` | `try` / `do`-`catch` | `try` / `catch` | — | **none yet** |
| **Numeric formatting** | `{}` any width | `\()` any | `{d}` any | `\()` any | **`@to_string` is i64-only** |
| **Ownership model** | ownership + **lifetimes** | ARC | manual | **MVS, no lifetimes** | **borrow/inout/sink, no lifetimes** |
| **Generics** | traits | protocols | **comptime** | generics | **comptime type functions** |
| **Rough LOC** | 14 | 12 | 22 | 14 | ~30 |

Two things jump out.

**1. Every peer language turns "might fail" into a value, and gives you an
operator to thread it.** EOF and a bad parse are `Option`/`?T`/error-union in all
four, propagated with `?`/`try`. Rue **traps** on both, which is why our program
can't write the natural `while there is another number { … }` loop and instead
demands a leading count. This is not a small ergonomic tax — it changes the
*shape* of the program. It is, by a wide margin, the biggest gap, and it is
exactly [RUE-6](https://linear.app/steve-klabnik/issue/RUE-6) (Option/Result
returns for `@parse_*`/`@read_line`, plus the `?`-operator).

**2. The design Rue is reaching for is Hylo's, not Rust's.** Rue and Hylo share
the whole ownership story — mutable value semantics, `borrow`/`inout`/`sink`
(= `let`/`inout`/`sink`), second-class references, **no lifetime annotations**.
The `v.push(inout self)` line *feels* the same in both. That's the bet: Hylo-style
simplicity over Rust-style lifetime power, with Zig-style `comptime` generics
underneath (`Vec(T)`/`Option(T)` are comptime type functions, just like Zig).
The comparison is reassuring — that combination is coherent, and where Rue is
recognizably its own thing (comptime + value semantics) it reads cleanly.

## What this says to build next

Crucially, **none of Rue's friction here is a design problem** — it's all
unfinished implementation, already on the roadmap:

- **Error handling** ([RUE-6](https://linear.app/steve-klabnik/issue/RUE-6)) —
  the headline. Make `@parse_*` return `Option`/`Result` and `@read_line` signal
  EOF, then ship the `?`-operator. This single feature closes the largest felt
  gap and turns the count-prefix workaround into a real read-until-EOF loop.
- **A prelude** ([RUE-287](https://linear.app/steve-klabnik/issue/RUE-287)) —
  `Option` should not need an `@import`.
- **`@to_string` for every integer width**
  ([RUE-314](https://linear.app/steve-klabnik/issue/RUE-314)).
- **`Vec` of non-scalar elements**
  ([RUE-311](https://linear.app/steve-klabnik/issue/RUE-311)) and real `Option(T)`
  accessors ([RUE-313](https://linear.app/steve-klabnik/issue/RUE-313)).

With RUE-6 and a prelude, this program in Rue would be about as short as the
Swift version — and would read like the language it's trying to be. That is the
most encouraging result of the exercise: the distance between Rue-today and
Rue-as-intended is a short list of tracked, non-design work, and dogfooding just
put them in priority order.
