+++
title = "Putting It Together"
weight = 10
template = "tutorial/page.html"
+++

# Putting It Together

Let's combine the pieces from the tutorial into a small command-line program:
read integers from standard input, then print how many were read, their sum, and
the maximum value.

This is closer to the kind of program Rue is trying to make pleasant than a
fixed-size algorithm demo. It uses input, parsing, optional values, matching,
loops, mutable accumulator state, string concatenation, and `println`.

## The Program

```rue check
fn Option(comptime T: type) -> type {
    enum { Some(T), None }
}

fn read_num() -> Option(i64) {
    let line = @read_line()?;
    @parse_i64(line)
}

fn main() -> i32 {
    let OptInt = Option(i64);

    let mut count: i64 = 0;
    let mut sum: i64 = 0;
    let mut max: OptInt = OptInt::None;

    loop {
        match read_num() {
            OptInt::Some(x) => {
                count = count + 1;
                sum = sum + x;
                max = match max {
                    OptInt::None => OptInt::Some(x),
                    OptInt::Some(m) => if x > m { OptInt::Some(x) } else { OptInt::Some(m) },
                };
            },
            OptInt::None => break,
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

## What It Does

The program reads one line at a time:

- `@read_line()` returns `Option(StrBuf)`: `Some(line)` for input, `None` at EOF.
- `@parse_i64(line)` returns `Option(i64)`: `Some(n)` for a valid integer,
  `None` for a line that is not an `i64`.
- The `?` operator in `read_num` returns early with `None` if either operation
  fails.

The main loop stops on the first `None`. That means it reads numbers until
end-of-input or the first non-number line.

## Running It

Save the program as `stats.rue`, then run it with the repository wrapper:

```bash
printf '7\n5\n1\n' | scripts/rue exec stats.rue
```

Output:

```text
count: 3
sum: 13
max: 7
```

This version returns the count as its process exit code, so the sample run exits
with status `3` after printing the output above.

The complete checked-in version lives at
[`examples/first/stats.rue`](https://github.com/rue-language/rue/blob/trunk/examples/first/stats.rue).

## Current Rough Edges

There is no prelude yet, so `Option` is not automatically in scope. This example
defines a small generic `Option(T)` inline because `read_num` names
`Option(i64)` in its return type. As the standard library and type-position
imports mature, tutorial code should move toward the explicit standard-library
form taught in the modules and arrays chapters.

## More Examples

The [GitHub repository](https://github.com/rue-language/rue) has more examples
in the `examples/` directory:

- `examples/first/stats.rue` - Streaming integer statistics
- `examples/std/arraybuf_demo.rue` - Growable buffers with `std.arraybuf.ArrayBuf`
- `examples/fibonacci.rue` - Iterative and recursive Fibonacci
- `examples/binary_search.rue` - Binary search on a sorted array
- `examples/structs.rue` - Working with points and rectangles

## Next Steps

You've learned the current core of Rue. For the complete language reference,
read the [Language Specification](/spec/).

Rue is still in early development. If you find bugs or have ideas, please
[file an issue](https://github.com/rue-language/rue/issues).
