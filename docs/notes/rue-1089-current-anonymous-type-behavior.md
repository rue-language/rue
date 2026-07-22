# Current anonymous-type behavior (pre-RUE-1089 historical record)

This note is the executable historical record of the **current, structural**
anonymous-`struct`/`enum` identity semantics that RUE-1088 (ADR-0066) specifies
and RUE-1089 (the "D1" cut) implements away. Every program below was compiled
and run against the debug compiler built from this worktree; the captured output
is verbatim, not hypothesized.

Capture provenance:

- Worktree checkout: `f0545a8f131907631e38a98cbb0d01bb9815459b` (detached).
- Compiler: `scripts/rue-bin` debug build (`//crates/rue:rue`).
- Target: `x86-64-linux`, internal linker (host defaults).
- Each program is a single self-contained root file; none of these scenarios
  needs `RUE_STD_PATH`/`std`. Commands were run from the directory holding the
  `.rue` file, so diagnostics show the bare filename.

The invocation for every scenario is:

```console
$ RUE="$(scripts/rue-bin)"
$ "$RUE" <file>.rue -o <file>      # compile
$ ./<file>; echo "run-exit: $?"    # run (only where it compiles)
```

The governing current rules are the **structural** forms of spec 4.14:8,
4.14:15, 4.14:21, and 4.14:25 (`docs/spec/src/04-expressions/14-comptime.md`).
ADR-0066 replaces them with **producer-nominal** identity: an anonymous type's
identity is its selected declaration expression under its static enclosing
comptime specialization, never a structural comparison of fields, variants,
method signatures, or bodies.

---

## Scenario 1 — two constructors, methodless structurally identical structs

Two distinct constructors each select their own anonymous `struct { x, y }`
declaration expression. Under current structural rules (spec 4.14:8) they are
**the same type**, so a value made through one is assignable to the other.

```rue
fn make_point1() -> type { struct { x: i32, y: i32 } }
fn make_point2() -> type { struct { x: i32, y: i32 } }

fn main() -> i32 {
    let P1 = make_point1();
    let P2 = make_point2();
    let p1: P1 = P1 { x: 10, y: 20 };
    let p2: P2 = p1;
    p2.x + p2.y
}
```

```console
$ "$RUE" s1.rue -o s1
Compiled s1.rue -> s1 (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s1; echo "run-exit: $?"
run-exit: 30
```

Currently: compiles; the cross-producer assignment `let p2: P2 = p1;` is
accepted; runs and returns `30`.

**Under ADR-0066:** this **changes to a compile error.** `make_point1` and
`make_point2` are different producers, so `P1` and `P2` are different types and
`let p2: P2 = p1;` is a deterministic type-mismatch (the shape of the amended
4.14:8 example). This is the reference example of the cut.

---

## Scenario 2 — identical fields and method *signatures*, different method *bodies*

Both `A` and `B` select `struct { x: i32, fn get(self) -> i32 { … } }` with the
same field and the same `get` signature, differing only in the method **body**
(`self.x` vs `self.x + 1`). Under current rules (spec 4.14:15) method bodies do
not affect equality, so `A()` and `B()` are the same structural type. Which body
actually runs is then decided by the compiler's **reached stable-representative**
selection, not by which constructor built the value.

Two variants isolate that selection. Both call `b.get()` on a value built
through `B` (`self.x + 1`); they differ only in whether `A` is *also* reached.

Variant 2a — only `B` is reached:

```rue
fn A() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x } }
}
fn B() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x + 1 } }
}
fn main() -> i32 {
    let TB = B();
    let b: TB = TB { x: 42 };
    b.get()
}
```

```console
$ "$RUE" s2_bonly.rue -o s2_bonly
warning: unused function 'A'
 --> s2_bonly.rue:1:1
  |
1 | / fn A() -> type {
2 | |     struct { x: i32, fn get(self) -> i32 { self.x } }
3 | | }
  | |_-
  |
  = help: if this is intentional, prefix it with an underscore: `_A`
Compiled s2_bonly.rue -> s2_bonly (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s2_bonly; echo "run-exit: $?"
run-exit: 43
```

Variant 2b — `A` is *also* reached (through `also_reach_a`), still calling the
`B`-built value's `get`:

```rue
fn A() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x } }
}
fn B() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x + 1 } }
}
fn also_reach_a() -> i32 {
    let TA = A();
    let a: TA = TA { x: 0 };
    a.get()
}
fn main() -> i32 {
    let TB = B();
    let b: TB = TB { x: 42 };
    also_reach_a();
    b.get()
}
```

```console
$ "$RUE" s2_ab.rue -o s2_ab
Compiled s2_ab.rue -> s2_ab (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s2_ab; echo "run-exit: $?"
run-exit: 42
```

Currently: the identical `b.get()` call on a `B`-constructed value returns `43`
when only `B` is reached (variant 2a: `B`'s own body runs) but `42` when `A` is
also reached (variant 2b). Reaching an unrelated producer `A` makes `A`'s body
the representative for the shared structural type, so `A`'s body (`self.x`)
executes for the `B`-built value. The value's constructor does not decide which
body runs.

*Implementation-defined vs specified:* spec 4.14:15 only specifies that method
bodies do **not** affect structural equality. It does **not** specify which body
executes; that `A` (rather than `B`) is chosen is the compiler's stable-minimum
representative policy — an implementation detail, and precisely the observable
non-locality ADR-0066 removes.

**Under ADR-0066:** `A()` and `B()` are different producer-nominal types.
`b.get()` on a `B`-built value always runs `B`'s body (`43`) regardless of
whether `A` is reached; a value of one cannot be assigned to the other. No
representative is chosen because there is no shared type.

---

## Scenario 3 — an error in one producer's method body, retracted when a peer is reached

This is the standalone reproduction of
`crates/rue-air/src/sema/tests.rs::late_anonymous_representative_retracts_stale_method_errors`.
`B`'s `get` body contains an error (`missing()`, an undefined function). `A`'s
`get` body is well-formed with the same field and signature. Whether the whole
program compiles depends on whether `A` is reached and becomes the stable
representative — which retracts the diagnostics of `B`'s now-abandoned body.

Variant 3a — only `B` is reached (its erroneous body is the representative):

```rue
fn B() -> type {
    struct {
        value: i32,
        fn get(self) -> i32 { missing() }
    }
}
fn A() -> type {
    struct {
        value: i32,
        fn get(self) -> i32 { self.value + 10 }
    }
}
fn main() -> i32 {
    let Item = B();
    let value: Item = Item { value: 10 };
    value.get()
}
```

```console
$ "$RUE" s3_b_only.rue -o s3_b_only
error: [E0202]: undefined function 'missing'
 --> s3_b_only.rue:4:31
  |
4 |         fn get(self) -> i32 { missing() }
  |                               ^^^^^^^^^
  |
compile-exit: 1
```

Variant 3b — `A` is also reached (through `discover(i32)`), becomes the
stable-minimum representative, and `B`'s stale error is retracted:

```rue
fn B() -> type {
    struct {
        value: i32,
        fn get(self) -> i32 { missing() }
    }
}
fn A() -> type {
    struct {
        value: i32,
        fn get(self) -> i32 { self.value + 10 }
    }
}
fn discover(comptime T: type) -> i32 {
    let Item = A();
    let value: Item = Item { value: 10 };
    value.get()
}
fn main() -> i32 {
    let Item = B();
    let value: Item = Item { value: 10 };
    discover(i32);
    value.get()
}
```

```console
$ "$RUE" s3_a_and_b.rue -o s3_a_and_b
Compiled s3_a_and_b.rue -> s3_a_and_b (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s3_a_and_b; echo "run-exit: $?"
run-exit: 20
```

Currently: the *identical* `B` producer with an erroneous method body makes the
program fail to compile (variant 3a, `E0202`) or compile and return `20`
(variant 3b) depending only on whether the unrelated producer `A` is reached.
The error is real diagnostic output tied to a body that is abandoned once a
lower representative is discovered.

*Implementation-defined:* the retraction, and its dependence on reachability of
a peer producer, is entirely an artifact of representative selection; the spec
does not describe it. It is the incremental-locality hazard ADR-0066 cites.

**Under ADR-0066:** `B`'s body is always checked on its own (it owns its
producer-nominal type), so variant 3a and variant 3b both report the `missing()`
error deterministically. Reaching `A` cannot retract `B`'s diagnostics; the two
producers never share a representative.

---

## Scenario 4 — cross-constructor anonymous enum equality

### 4a — same constructor and specialization (`Option(i32)` twice)

Both `A` and `B` select the same `Option` producer at the same `i32`
specialization. This is the `anon_enum_structural_reuse` shape (spec 4.14:21).

```rue
fn Option(comptime T: type) -> type { enum { Some(T), None } }
fn main() -> i32 {
    let A = Option(i32);
    let B = Option(i32);
    let x: A = A.Some(10);
    let y: B = x;
    match y { B.Some(n) => n, B.None => 0 }
}
```

```console
$ "$RUE" s4_same.rue -o s4_same
Compiled s4_same.rue -> s4_same (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s4_same; echo "run-exit: $?"
run-exit: 10
```

Currently: compiles; `x` (type `A`) is assignable to `y` (type `B`); runs and
returns `10`.

**Under ADR-0066:** **unchanged (still compiles).** `A` and `B` select the same
producer *and* the same canonical specialization `Option(i32)`, so they denote
the same producer-nominal type. This is the renamed "same-producer reuse" case
in the amended 4.14:21 example.

### 4b — two different constructors, identical variants

`First` and `Second` are distinct producers that each declare
`enum { Some(i32), None }`. Under current structural rules they are the same
type.

```rue
fn First() -> type { enum { Some(i32), None } }
fn Second() -> type { enum { Some(i32), None } }
fn main() -> i32 {
    let A = First();
    let B = Second();
    let x: A = A.Some(42);
    let y: B = x;
    match y { B.Some(n) => n, B.None => 0 }
}
```

```console
$ "$RUE" s4_diff.rue -o s4_diff
Compiled s4_diff.rue -> s4_diff (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s4_diff; echo "run-exit: $?"
run-exit: 42
```

Currently: compiles; the cross-producer assignment `let y: B = x;` is accepted;
runs and returns `42`.

**Under ADR-0066:** this **changes to a compile error.** `First` and `Second`
are different producers, so their `enum { Some(i32), None }` types are distinct
and non-assignable — a deterministic type mismatch. ADR-0066 explicitly adds
this different-producer, same-shape enum mismatch as new coverage.

---

## Scenario 5 — forwarding constructor (preserved behavior)

A constructor that *forwards* an existing type value does not mint a new
producer; it preserves the forwarded type's identity. This is the same under
current rules and under ADR-0066 — recorded here explicitly as a **preserved**
behavior, not a change.

### 5a — plain forward `fn F() -> type { G() }`

```rue
fn G() -> type { struct { x: i32, y: i32 } }
fn F() -> type { G() }
fn main() -> i32 {
    let TG = G();
    let TF = F();
    let g: TG = TG { x: 40, y: 2 };
    let f: TF = g;
    f.x + f.y
}
```

```console
$ "$RUE" s5_forward.rue -o s5_forward
Compiled s5_forward.rue -> s5_forward (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s5_forward; echo "run-exit: $?"
run-exit: 42
```

### 5b — `fn Id(comptime T: type) -> type { T }` (ADR-0066 4.14:22 example)

```rue
fn Id(comptime T: type) -> type { T }
fn Pair(comptime T: type) -> type { struct { first: T, second: T } }
fn main() -> i32 {
    let P = Pair(i32);
    let Q = Id(P);
    let p: P = P { first: 20, second: 22 };
    let q: Q = p;
    q.first + q.second
}
```

```console
$ "$RUE" s5_id.rue -o s5_id
Compiled s5_id.rue -> s5_id (target: x86-64-linux, linker: internal)
compile-exit: 0
$ ./s5_id; echo "run-exit: $?"
run-exit: 42
```

Currently: both compile and return `42`. `F()` denotes the same type as `G()`;
`Id(P)` denotes the same type as `P`.

**Under ADR-0066:** **PRESERVED — no change.** `F` and `Id` do not *select* an
anonymous declaration expression; they return an existing type value, so they
forward its identity rather than minting a new producer (amended 4.14:22 /
4.14:25, and ADR-0066's "forwarding constructor" acceptance-matrix row). Note
the distinction from Scenario 1: `make_point1`/`make_point2` each *select* their
own `struct { … }` expression and therefore each mint a distinct producer, while
`F`/`Id` forward and do not.

---

## Scenario 6 — structurally-bounded anonymous-method specialization recursion

This is the standalone reproduction of
`crates/rue-cli-tests/cases/lazy_specialization_references.toml::alternating_specialization_method_recursion_is_bounded`.
`runaway(n)` builds `Wrapper(n)` and calls its `go` method, whose body calls
`runaway(n + 1)`. Under the current **structural** rules every `Wrapper(n)`
declares the same `struct { value: i32, fn go(self) -> i32 { … } }`, so they all
collapse to one structural type. The captured comptime `N` belongs to the
stable-minimum representative body (the `min` producer, `N = 0`), so the shared
`go` always calls `runaway(1)`, the reached set is finite (`runaway(0)`,
`runaway(1)`, and one representative `Wrapper` method), and the program
**compiles** even though it would recurse forever at runtime (hence the case is
`compile_only`).

```rue
fn Wrapper(comptime N: i32) -> type {
    struct {
        value: i32,
        fn go(self) -> i32 { runaway(N + 1) }
    }
}

fn runaway(comptime n: i32) -> i32 {
    let W = Wrapper(n);
    let w = W { value: n };
    w.go()
}

pub fn start() -> i32 { runaway(0) }
```

```console
$ "$RUE" main.rue -o s6      # main.rue @imports the lib.rue above
Compiled main.rue -> s6 (target: x86-64-linux, linker: internal)
compile-exit: 0
```

Currently: **compiles** (exit `0`, no diagnostics). The genuinely infinite
`runaway(n) -> Wrapper(n).go() -> runaway(n + 1)` recursion is *bounded at
compile time* only because structural representative convergence collapses every
`Wrapper(n)` onto one representative whose `go` body calls `runaway(1)`.

*Implementation-defined:* that the recursion terminates at all is an artifact of
stable-representative selection over the captured `N`; the spec does not describe
it. It is precisely the non-local convergence ADR-0066 removes.

**Under ADR-0066:** this **changes to a compile error.** Each `Wrapper(n)` is a
distinct producer-nominal type, so `Wrapper(n).go()` always calls `runaway(n+1)`
with its own `n`; the recursion instantiates a new specialization at every step
and is diagnosed by the specialization-depth limit (spec 4.14:18) with `E1200`
("exceeded the maximum nesting depth"). The CLI case is converted from
`compile_only` to a `compile_fail` E1200 case retaining this program.

---

## Summary of dispositions

| Scenario | Current behavior (captured) | Under ADR-0066 producer-nominal rules |
| --- | --- | --- |
| 1. Two ctors, identical methodless structs | compiles, cross-assign OK, exit `30` | **compile error** (distinct producers) |
| 2. Same signatures, different bodies | body that runs depends on which producers are reached (`43` alone / `42` with peer) | each producer runs its own body; no representative; cross-assign is an error |
| 3. Error in one body | fails alone (`E0202`) / compiles with peer reached (exit `20`) | error reported deterministically in both variants |
| 4a. Same enum ctor+specialization | compiles, exit `10` | **unchanged** (same producer + specialization) |
| 4b. Two enum ctors, same variants | compiles, cross-assign OK, exit `42` | **compile error** (distinct producers) |
| 5. Forwarding ctor (`F()->{G()}`, `Id(T)->{T}`) | compiles, exit `42` | **preserved** (forwards identity, mints nothing) |
| 6. Anon-method specialization recursion | compiles (bounded by structural representative convergence) | **compile error** `E1200` (each `Wrapper(n)` distinct → unbounded specialization depth) |

Scenarios 1, 2, 3, and 4b are the observable non-localities the cut removes:
same-shape distinct producers are collapsed, and which method/destructor body
executes (or whether its diagnostics survive) depends on the reached set rather
than on the source producer. Scenarios 4a and 5 are the cases the new rules
deliberately keep.
