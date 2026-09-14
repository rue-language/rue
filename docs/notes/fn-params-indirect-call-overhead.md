# Indirect-call overhead of second-class function parameters

Status: reference measurement, 2026-09-14, taken for RUE-2113 (ADR-0096 phase
4) before the `fn_params` preview gate was removed. The RUE-2107 exit criteria
asked for the overhead of the indirect call and the generated instance count
to be measured rather than promised; this note is that measurement. Current
source is authoritative for the mechanism; the numbers are one host's.

## Result

- **A call through a `fn` parameter costs the same as a direct call that is
  not inlined.** At `-O0`, where neither loop inlines its callee, 400 million
  indirect calls cost 3% more than 400 million direct calls, inside the run
  to run noise of the host.
- **Against an inlined direct call the indirect call costs about 0.6 ns per
  call on this host.** At `-O2` the direct loop inlines its callee and the
  indirect loop cannot (the compiler never devirtualizes a callback, ADR-0096
  decision 6), so the difference is the cost of a call plus the lost
  inlining: 0.23 s over 400 million calls, about 1.2 cycles at 2.1 GHz.
- **A comparator sort pays about 6%.** `std.sort.sort_by` over 2 million
  pseudo-random `i64` with `a < b` reached through the callback is 6% slower
  than `std.sort.quicksort` with `<` inline, at `-O2`: 0.741 s against
  0.697 s, roughly 0.7 ns per comparison over the 60 million comparisons a
  quicksort of that size makes.
- **One instance per element and context type, not per callback.** Three
  different comparators bound to `sort_by(i64, Policy, ...)` in one program
  generate one `sort_by`, one `qsort_range_by` and one `median_of_three_by`
  instance between them; only the `(T, C)` pair multiplies the generated
  code. A design that took the comparator as a `comptime` function would
  generate the three sorts three times.

## The host

Four cores of an Intel Xeon at 2.10 GHz with 15 GiB, otherwise idle, Linux
x86-64. Every program was compiled by the compiler at the RUE-2113 head with
the standard library in the tree, linked with the internal linker, and run
fifteen times; the tables report the minimum and the median of the process's
user CPU time, which agreed with wall-clock time to the millisecond.

## The call loop

The callee is a three-shift xorshift step so that an iteration is a few
instructions and the call dominates. The direct program calls it by name; the
indirect program binds it to a `fn(u64, u64) -> u64` parameter of the loop
function and calls through the parameter. Both loops run 400 million
iterations and exit with the low bits of the result, which both programs
agree on.

```rue
fn mix(x: u64, i: u64) -> u64 {
    let y = x ^ (x << 13);
    let z = y ^ (y >> 7);
    (z ^ (z << 17)) + i
}

// direct                              // indirect
fn run(n: u64) -> u64 {                fn run(n: u64, step: fn(u64, u64) -> u64) -> u64 {
    let mut x: u64 = 88172645463325252;    let mut x: u64 = 88172645463325252;
    let mut i: u64 = 0;                    let mut i: u64 = 0;
    while i < n {                          while i < n {
        x = mix(x, i);                         x = step(x, i);
        i += 1;                                i += 1;
    }                                      }
    x                                      x
}                                      }
```

| program | opt | min | median | per call against the `-O0` direct call |
| --- | --- | --- | --- | --- |
| direct | `-O0` | 0.994 s | 1.119 s | — |
| indirect | `-O0` | 1.027 s | 1.234 s | +0.08 ns |
| direct | `-O2` | 0.733 s | 0.869 s | −0.65 ns (callee inlined) |
| indirect | `-O2` | 0.961 s | 1.095 s | −0.08 ns |

`--emit cfg -O2` confirms the mechanism: the direct loop's body contains no
call at all, and the indirect loop's body contains one `call_indirect` per
iteration. The indirect call at `-O2` is therefore as fast as the direct call
at `-O0`, and the whole `-O2` gap is inlining the callback cannot have.

## The sort

`sort_natural` fills an `ArrayBuf(i64)` with 2 million xorshift values and
calls `std.sort.quicksort(i64, inout v)`; `sort_by` fills the same values and
calls `std.sort.sort_by(i64, Natural, borrow ctx, inout v, less)` with
`fn less(borrow ctx: Natural, a: i64, b: i64) -> bool { a < b }` and an empty
context struct. The two algorithms are the same code with `less(borrow ctx,
a, b)` in place of `a < b` (`std/sort.rue` explains why they cannot share one
body). Both verify the result with the matching `is_sorted` before exiting.

| program | min | median |
| --- | --- | --- |
| `sort_natural` (`quicksort`, `<` inline) | 0.697 s | 0.754 s |
| `sort_by` (`less` through the callback) | 0.741 s | 0.779 s |

## The instance count

A program that binds three comparators of type
`fn(borrow Policy, i64, i64) -> bool` to `std.sort.sort_by(i64, Policy, ...)`
and also calls `std.sort.quicksort(i64, ...)` emits 28 CFG functions in all.
The ones that belong to the sorts:

| instance | count |
| --- | --- |
| `sort_by.i64.Policy`, `qsort_range_by.i64.Policy`, `median_of_three_by.i64.Policy` | 1 each |
| `quicksort.i64`, `qsort_range.i64`, `median_of_three.i64` | 1 each |
| `_at.i64` (shared by both families) | 1 |
| the three comparators | 1 each |

The callback is a runtime value, so binding a fourth comparator adds one
function (the comparator) and no sort instances.

## What this does and does not say

The per-call figure is the cost on one out-of-order x86-64 core with a
perfectly predicted indirect branch, which a monomorphic call site in a hot
loop is. A site that alternates between callbacks will mispredict and pay
more; nothing here measures that. AArch64 was not timed: the AArch64 lowering
is the same call plan with `blr` in place of `call r64` (RUE-2195), and its
executables were cross-compiled and linked but not run on this host.

Devirtualization of a known target remains an optimization ADR-0096 permits
and the compiler does not perform. The `-O2` row above bounds what it would
buy for a loop whose callee is small enough to inline: about 0.6 ns per call
here, and nothing for a callee that would not be inlined anyway.
