+++
title = "Slices"
weight = 2
template = "spec/page.html"
+++

# Slices

A slice is a *view* over a contiguous run of elements that someone else owns.
It is the second rung of the fixed/slice/growable structure of ADR-0043: the
fixed rung is `[T; N]` (section 7.1), and the slice rung is `[T]`, specified
here.

## The Slice Type

{{ rule(id="7.2:1", cat="syntax") }}

```ebnf
slice_type = "[" type "]" ;
```

A slice type is the bracketed type form of 7.1:14 with the `";" array_length`
part omitted (Appendix A's `type` production covers both spellings in one rule).

{{ rule(id="7.2:2", cat="normative") }}

The type `[T]`, the *slice type* over element type `T`, denotes a view of a
contiguous run of `T` elements. The run's length is a **runtime** value carried
by the view itself, so — unlike `[T; N]`, whose length is part of its type
(3.5:1) — two `[T]` values of different lengths have the same type. A slice
does not own the elements it views: the viewed storage belongs to the fixed
array (or other backing collection) the view was taken from, and the view's
existence neither moves that storage nor extends its lifetime.

{{ rule(id="7.2:3", cat="normative") }}

A slice is *second-class*. It is a scoped capability to read the storage it
views, valid only for the duration of the call it is an argument to, and the
type system enforces that by admitting `[T]` in exactly one position: the type
of a function parameter (7.2:7). Every position that could let a view outlive
the storage it views — a return type, an aggregate field, a binding — is a
compile-time error (7.2:4, 7.2:5, 7.2:6). Because escape is impossible
structurally, Rue needs no lifetimes to keep a view from dangling (ADR-0043;
the access model is ADR-0037).

## Where a Slice May Appear

{{ rule(id="7.2:4", cat="legality-rule") }}

A slice type **MUST NOT** be a function's return type. Returning a view would
outlive the frame that owns the viewed storage, so `fn f() -> [T]` is a
compile-time error whether or not the function body could produce a view.

{{ rule(id="7.2:5", cat="legality-rule") }}

A slice type **MUST NOT** be the type of an aggregate field: neither a struct
field nor an enum tuple-variant payload. Storing a view in an aggregate would
let it escape wherever the aggregate goes, so either declaration is a
compile-time error at the item, independently of any use.

{{ rule(id="7.2:6", cat="legality-rule") }}

A slice type **MUST NOT** name a binding. Neither a `let` local nor a `const`
item may be annotated `[T]`; a view cannot be bound past the argument scope it
was materialized for, and either declaration is a compile-time error.

{{ rule(id="7.2:7", cat="normative") }}

The one position in which a slice type is legal is the type of a function
parameter — a free function's, a method's, or an associated function's. In that
position it is the universal read interface over the collection rungs: a
function written against `borrow s: [T]` accepts a view of any fixed array of
`T`, whatever its length.

{{ rule(id="7.2:8") }}

```rue
fn total(borrow s: [i64]) -> i64 {
    let mut acc: i64 = 0;
    let mut i: u64 = 0;
    while i < s.len() {
        acc = acc + s[i];
        i = i + 1;
    }
    acc
}

fn main() -> i32 {
    let a: [i64; 3] = [10, 20, 12];
    let b: [i64; 5] = [1, 1, 1, 1, 1];
    let v: i64 = total(borrow a) + total(borrow b);
    @intCast(v)  // 47
}
```

## Slice Parameter Modes

{{ rule(id="7.2:9", cat="normative") }}

A slice parameter is declared with the `borrow` parameter mode — `borrow s:
[T]` — which is the *shared* view: the callee may read the viewed elements and
**MUST NOT** write them. The mode belongs to the parameter, not to the type;
the view value the parameter receives is itself passed by value (7.2:24).

{{ rule(id="7.2:10", cat="legality-rule") }}

A call **MUST** supply a slice parameter with a `borrow` argument, and the
parameter it supplies **MUST** be declared `borrow`. The exclusive `inout [T]`
form of ADR-0043 is not yet implemented: an `inout [T]` parameter — and an
unmarked `[T]` parameter — may be *declared*, but no call can supply an
argument for it, because materializing a view from a fixed array produces only
the shared form (7.2:12) while the argument-mode rule (4.10:3) demands the mode
the parameter declares. Every such call is a compile-time error, whichever
argument mode it writes. Forwarding between two `inout [T]` parameters is
consequently unreachable.

{{ rule(id="7.2:11", cat="legality-rule") }}

An element write through a slice — `s[i] = e` — is **not accepted** in any
parameter mode, so a slice is a read-only view in this specification. Writing
through the shared `borrow` form is a mutation of borrowed storage and is a
compile-time error; writing through the `inout` form is the unimplemented
exclusive-view case of 7.2:10, whose parameter no call can reach. This rule
states the current legality and does not fix which diagnostic reports it.

## Fixed-Array-to-Slice Coercion

{{ rule(id="7.2:12", cat="normative") }}

A `borrow` argument whose parameter type is `[T]` and whose operand is a
`[T; N]` place undergoes the *fixed-array-to-slice coercion*: the caller
materializes the two-word view — the address of the array's element `0` and the
length `N` — and passes it to the callee. This is the argument-position
coercion that 4.10:4 defers to this section, and the view-materialization case
of the `borrow` calling convention (6.1:26 item 1). The coercion applies to
argument position only; it is not a general subtyping of `[T; N]` to `[T]`.

{{ rule(id="7.2:13", cat="legality-rule") }}

The operand of the coercion **MUST** denote a place holding a *whole* fixed
array whose element type is exactly the slice's element type. A local
variable, a by-ref parameter, and an array-typed struct field all qualify; a
temporary — such as a call result — does **not**, so a `borrow` argument that
denotes no place is a compile-time error here rather than being elaborated into
a place as it would be for an ordinary `borrow` parameter (4.10:10, 6.1:39).
A subrange of an array is not a coercion operand either: range slicing is not
part of this specification. An element type that merely converts to the slice's
element type is a compile-time type mismatch, not a coercion.

{{ rule(id="7.2:14", cat="legality-rule") }}

A non-empty fixed array whose element type is not slot-identical in layout —
an element narrower than a stack slot, such as `i32` or `u8` — **MUST NOT**
coerce to a slice. A view strides by the element's own size, which for such an
element differs from the stride of the frame array it would view, so the
coercion is refused at the argument with a diagnostic naming the element type.
This is a restriction of the current implementation, not a property of the
slice type. An **empty** array is exempt: `[T; 0]` coerces for every element
type, because a zero-length view's pointer word is never dereferenced (7.2:22).

{{ rule(id="7.2:15", cat="normative") }}

An argument that is *already* a slice — `f(borrow s)` where `s` is a slice
parameter — is not re-materialized. The existing two-word view is read out and
passed through by value, so a view forwarded through any number of calls
continues to describe the storage the original coercion viewed.

{{ rule(id="7.2:16") }}

```rue
struct Row { cells: [i64; 3] }

fn head(borrow s: [i64]) -> i64 {
    s[0]
}

fn forward(borrow s: [i64]) -> i64 {
    head(borrow s)  // forwarded, not re-materialized
}

fn main() -> i32 {
    let r = Row { cells: [42, 1, 2] };
    let v: i64 = forward(borrow r.cells);  // a field place coerces
    @intCast(v)  // 42
}
```

## Slice Length

{{ rule(id="7.2:17", cat="normative") }}

The method call `s.len()` on a slice `s` evaluates to the view's runtime length
— the number of elements it views — as a value of type `u64`. For a view
materialized from a `[T; N]` place the length is `N`.

{{ rule(id="7.2:18", cat="normative") }}

`len` is the only method a slice has. Any other method name on a slice receiver
is a compile-time error; there is no user-defined method resolution on `[T]`.

{{ rule(id="7.2:19") }}

```rue
fn ln(borrow s: [i64]) -> i32 {
    @intCast(s.len())
}

fn main() -> i32 {
    let a: [i64; 6] = [9, 9, 9, 9, 9, 9];
    let empty: [i64; 0] = [];
    ln(borrow a) + ln(borrow empty)  // 6 + 0
}
```

## Slice Indexing

{{ rule(id="7.2:20", cat="normative") }}

An index expression `s[i]` on a slice reads the element at position `i` of the
viewed run. Positions are numbered from `0` in ascending address order, matching
the array layout of 3.5:4, so `s[i]` denotes the same element as `a[i]` for a
view materialized from the whole of `a`. The read is a use of the viewed
storage, not of the view: it copies the element out and leaves both the view and
the backing array unchanged.

{{ rule(id="7.2:21", cat="legality-rule") }}

The index **MUST** be of an integer type, signed or unsigned, exactly as for
array indexing (7.1:7).

{{ rule(id="7.2:22", cat="dynamic-semantics") }}

A slice index is bounds-checked against the view's runtime length at every
access. An index that is out of range — `i` negative, or `i ≥ s.len()` — **MUST**
trap before the element is read, halting the program with exit code 101, the
same abort discipline as an out-of-range array index (7.1:11, 8.2:2). Because
the length is a runtime value, the check is always dynamic: there is no
compile-time bounds rule for a slice corresponding to 7.1:9.

{{ rule(id="7.2:23") }}

```rue
fn get(borrow s: [i64], i: u64) -> i32 {
    @intCast(s[i])
}

fn main() -> i32 {
    let a: [i64; 3] = [10, 20, 30];
    get(borrow a, 5)  // traps: 5 >= 3, exit code 101
}
```

## Representation

{{ rule(id="7.2:24", cat="normative") }}

A slice value is a two-word view — a pointer to the first viewed element and a
runtime length — and is passed **by value**, not by reference (6.1:26 item 1).
A view is freely copyable: passing it does not consume it, and one backing array
may be viewed by several shared slice arguments of the same call at once.

{{ rule(id="7.2:25", cat="informative") }}

The type `str` is the byte-string refinement of `[u8]`: it carries the slice
rung's shape plus the byte-string convention of ADR-0035. Its own rules — the
first-class static-backed `str`, the `borrow str` / `inout str` views, and the
coercions between them — are specified in 3.7:43 and following, and are not
restated here.
