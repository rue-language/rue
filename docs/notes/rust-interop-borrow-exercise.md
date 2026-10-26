# Rust interop: can Rue use Rust's std without lifetimes?

Status: exploratory analysis, 2026-10-26, for the Rust-interop redesign
discussion. No Linear issue and no decision. The ports were checked with
rustc 1.92.0 (edition 2024). The survey reads the rustdoc JSON that
nightly 1.101.0 (2026-10-24) ships for core, alloc and std. The reproduction
kit is in [rust-interop-borrow-exercise/](rust-interop-borrow-exercise/).

## Question

If Rue is redesigned so that Rust and Rue share one IR and Rue's standard
library is Rust's, Rue code has to call APIs like `str::split`,
`HashMap::entry` and `thread::scope`, and Rue libraries have to offer APIs of
the same shape. The borrow rule proposed for that design is:

- **R1.** Inside a function body, borrows are first-class. A local can hold
  `&T`, `Option<&T>`, `Iter<'_, T>`, a `MutexGuard`, or a closure that captures
  by reference. An intraprocedural region checker (NLL-class) enforces this.
- **R2.** A Rue signature mentions borrows only in the shapes Rust's
  lifetime-elision rules produce. Rue source never names a lifetime.
- **R3.** A Rue-defined type has no lifetime parameters.

Calls are not restricted. The R1 checker handles any callee signature, so Rue
code can call a Rust function whose own signature names lifetimes. The
question is whether this rule is enough to write real programs against
Rust's std, and to write std-shaped libraries.

## Result

- **Calling std was never the problem.** Seven programs were ported onto
  Rust's std: five Rue examples, `std.parallel.join_inout`, and a std-style
  collection. All seven fit R1 through R3. They call APIs whose own
  signatures name lifetimes (`thread::scope`, `VacantEntry::insert`,
  `Formatter::debug_map`), and none of them needed a lifetime to be named.
  Each port has a Rust twin that rustc accepts with no named lifetime outside
  clearly marked regions. A checker script verifies that.
- **92% of std's own function signatures could be declared under R2.** Std has
  2,235 distinct stable signatures. Of those, 42.8% mention no borrow, 31.0%
  borrow only in inputs, and 18.5% return a borrow in an elision shape. Another
  5.8% are methods on types that carry lifetimes, which R3 rules out. The
  remaining 2.0% (44 signatures) need a named lifetime. 13 of those are
  `unsafe` raw-pointer conversions such as `slice::from_raw_parts`.
- **R3 is the binding constraint.** 146 of std's 406 public types (36%) carry a
  lifetime, and 101 of those are iterators or cursors. In the ports it cost
  less than the
  number suggests, because a borrowing iterator can be returned as
  `impl Iterator` built from a closure, with no named type. The real
  casualties were:
  - structs that borrow their input (a lexer, a parser, a zero-copy JSON
    tree);
  - `for x in &my_collection` for a user collection;
  - a result that borrows from two parameters.
- **Today's borrow workarounds disappear.** Several programs work around the
  fact that a borrow cannot be held or returned:
  - six larger examples each carry a 108 to 173 line string pool;
  - wordfreq keeps a `FirstSeen` byte pool;
  - bst stores its tree in an index arena with copy-out/copy-in.

  Under R1 and R2, wordfreq's pool becomes a `HashMap<&[u8], u64>` keyed by
  borrows of the input, and bst becomes an ordinary `Box` tree. The six string
  pools were not ported. Their headers give the same reason, though: records
  stay plain integers so arena elements can be read by copy, and gazette's
  cites RUE-651 directly. So they should go the same way.
- **A refined rule closes every gap the ports hit, and source still names no
  lifetime.** It has three parts:
  - *Borrowing types*: a struct or enum that holds borrows, with one
    implicit lifetime. They cover 140 of the 146 lifetime-carrying std types.
  - *Parameter-named dependencies*, written `-> borrow(a, b) T`.
  - *Impl-header elision*, which lets `for x in &map` work.

  Two shapes remain that Rue can call but not declare: lifetimes inside
  closure bounds (`thread::scope`, `binary_search_by_key`), and types with two
  lifetimes (`core::fmt`'s Debug builders and `thread::Scope`).
- **The ports needed several features besides borrows.** Closures (six of the
  seven ports), opaque return types, conformances with bodies, a spelling for
  `'static`, and one formatting builtin. `format_args!` was the only macro the
  twins could not avoid.
- **On borrows, the redesign holds up. The cost is real and reverses a
  ratified decision.** R1 means loans live in locals across statements.
  ADR-0037 and the formal core's decision 4 currently guarantee that a loan
  never outlives the call it is born in. R1 requires an NLL-class region
  checker and new metatheory for ADR-0097's safety proofs.

## Method

**Ports.** Seven programs, each written twice:

1. In the redesigned Rue dialect, shown below for wordfreq and bst in full
   and the others in excerpt.
2. As a Rust twin that rustc compiles and runs.

R1 through R3 are Rust's own borrow semantics plus two syntactic
restrictions. So a twin that rustc accepts, and that names no lifetime, is
evidence of two things: the Rue program is sound, and the rule admits it.
`twins/check.py` enforces the "no named lifetime" part. It reports any
lifetime outside a `// refined:` region, along with each use of `'static`
and every macro invocation. The twins avoid macros, so output uses
`write_all` and numbers use `to_string`.

**Survey.** `survey/classify.py` reads the rustdoc JSON and classifies
signatures, types, trait definitions and trait impls. It covers stable
items only, and leaves out deprecated items, `#[doc(hidden)]` items,
`core::arch`, intrinsics and portable SIMD.

- *Methods* means inherent methods and free functions. Trait-impl methods are
  excluded, because the trait fixes their signatures.
- *Distinct* collapses the copies of one signature that std stamps out for
  each integer type.
- The classes are:
  - C0: no borrow.
  - C1: borrows in inputs only.
  - C2: the output borrows, and elision would produce exactly this signature.
  - C3: the method's `Self` type has a lifetime parameter.
  - C4: a named lifetime is needed.

**Limits.**

- The twins show that the semantics are sound and sufficient. They do not
  show that a Rue checker could infer them.
- I chose the seven programs. The largest examples (caldera at 105K lines,
  gazette, harbor) were not ported.
- The survey works at the signature level and says nothing about
  ergonomics.
- The Rue compiler was not run. Output parity with the Rue originals was
  checked for three programs:
  - wordfreq reproduces the pinned stdout of `cli.examples_wordfreq`;
  - bst prints 42;
  - dijkstra exits with 42.

  The other twins check values written into the twin itself.

## The ports

| Program | Rue today | Port | R1–R3 | Needs the refined rule for |
| --- | --- | --- | --- | --- |
| `examples/wordfreq` | `FirstSeen` byte pool with offset and length arrays, because `ArrayBuf(StrBuf)` elements cannot be read in place (RUE-651) | `HashMap<&[u8], u64>` and `Vec<&[u8]>` borrowing the input; the entry API | yes | nothing |
| `examples/ruelex` (lexer core) | the lexer owns the source bytes; tokens are spans | strict: the same shape; refined: zero-copy `Lexer<'a>` and `Token<'a>` | yes | zero-copy tokens |
| `examples/bst.rue` | index arena, sentinel slot 0, copy-out/copy-in | `Box` tree; a `&mut` cursor for insert; a `Vec<&Node>` stack; an `iter::from_fn` iterator | yes | nothing |
| `examples/dijkstra` | hand-rolled binary heap | `BinaryHeap`; all data owned | yes | nothing |
| `examples/jsonfmt` | `std.json` | parser as free functions over `(src, &mut pos)`; owned tree; `impl Display` | yes | zero-copy `JsonRef<'a>`; `get_or` borrowing from two parameters |
| `std/parallel.rue` `join_inout` | wraps the `@join_inout` intrinsic | six-line wrapper over `thread::scope`; `chunks_mut` data parallelism; a `MutexGuard` in a local | yes | nothing |
| ordered map (library side) | approximated by `ruelex/interner.rue`, `wordfreq/firstseen.rue` and the `pool.rue` files | full std-style API | yes, except one impl | `IntoIterator for &OrderedMap` |

### Syntax this draft assumes

Each item below is new, or new in this form:

1. `@import("std")` is Rust's std. Rust generics take Rue's call syntax
   (`HashMap(K, V)`), and prelude names (`Some`, `None`, `Ok`, `Err`, `Vec`,
   `String`) need no qualification.
2. `borrow T` and `inout T` are types, spelling `&T` and `&mut T`. Today they
   exist only as parameter modes and as place-returning results.
3. A local can hold a borrow (R1). Writing through a borrow the local can be
   re-pointed uses `*`. First-class local borrows force a distinction between
   rebinding a borrow and writing through it, which today's place-based Rue
   never needed. The syntax is an open question.
4. Closures (`|x| ...`) and `impl Trait` in argument and return position.
5. A conformance can be satisfied by a struct's own methods for a Rust trait
   (`Json is std.fmt.Display`). Lowering generates the forwarding `impl`.
6. A string literal is a `borrow str` with static lifetime, and `+` on
   strings builds a `String`.

### wordfreq

Twin: `twins/wordfreq.rs`. Its output matches the pinned
`cli.examples_wordfreq` fixture byte for byte (`words=16`, `distinct=6`,
`top=the count=6`). Its usage, unreadable-file and empty-input paths give the
messages and exit codes the original's source specifies.

```rue
const std = @import("std");
const HashMap = std.collections.HashMap;
const Entry = std.collections.hash_map.Entry;

fn out(bytes: borrow [u8]) {
    let _ = std.io.stdout().write_all(bytes);
}

fn main() -> i32 {
    let path = match std.env.args().nth(1) {
        Some(p) => p,
        None => { out("usage: wordfreq <file>\n".as_bytes()); return 1; },
    };
    let mut data = match std.fs.read(borrow path) {
        Ok(d) => d,
        Err(_) => { out("error: cannot read input file\n".as_bytes()); return 1; },
    };
    data.make_ascii_lowercase();

    let mut counts: HashMap(borrow [u8], u64) = HashMap.new();  // R1: keys borrow `data`
    let mut order: Vec(borrow [u8]) = Vec.new();                // R1: first-seen order, no pool
    let mut total: u64 = 0;
    for w in data.split(|b| !b.is_ascii_alphabetic()) {         // R1: a borrowing iterator
        if w.is_empty() { continue; }
        total += 1;
        match counts.entry(w) {                                 // R1: Entry holds `inout counts`
            Entry.Occupied(mut e) => { *e.get_mut() += 1; },
            Entry.Vacant(e) => { e.insert(1); order.push(w); },
        }
    }
    out(("words=" + total.to_string() + "\n").as_bytes());
    out(("distinct=" + order.len().to_string() + "\n").as_bytes());

    // Earliest word wins ties: scan in first-seen order, replace only on a
    // strictly greater count.
    let mut best: Option((borrow [u8], u64)) = None;
    for w in order.iter().copied() {
        let c = counts[w];
        let replace = match best { Some((_, bc)) => c > bc, None => true };
        if replace { best = Some((w, c)); }
    }
    match best {
        Some((w, c)) => {
            out("top=".as_bytes());
            out(w);
            out((" count=" + c.to_string() + "\n").as_bytes());
        },
        None => {},
    }
    0
}
```

The original is 190 code lines across `main.rue` and `firstseen.rue`; the
twin is 54. Part of the difference is Rust's richer std (`fs::read`, `split`,
the entry API). The part that comes from the borrow rule is `firstseen.rue`.
It exists only because a word cannot be read back out of an
`ArrayBuf(StrBuf)` in place, so it disappears.

### bst

Twin: `twins/bst.rs`. It prints 42, like the original, then the sorted keys.

```rue
const std = @import("std");
const Box = std.boxed.Box;

struct Node {
    val: i64,
    left: Option(Box(Node)),
    right: Option(Box(Node)),
}

struct Tree {
    root: Option(Box(Node)),

    fn new() -> Self { Self { root: None } }

    fn insert(inout self, v: i64) {
        let mut cur = inout self.root;          // R1: a local borrow, re-pointed below
        loop {
            match cur {
                Some(node) => { cur = if v < node.val { inout node.left } else { inout node.right }; },
                None => break,
            }
        }
        *cur = Some(Box.new(Node { val: v, left: None, right: None }));
    }

    fn find(borrow self, v: i64) -> Option(borrow Node) {   // R2: borrows from self
        let mut cur = self.root.as_deref();
        loop {
            match cur {
                Some(n) => {
                    if v == n.val { return Some(n); }
                    cur = if v < n.val { n.left.as_deref() } else { n.right.as_deref() };
                },
                None => return None,
            }
        }
    }

    fn sum(borrow self) -> i64 {
        let mut stack: Vec(borrow Node) = Vec.new();         // R1: a Vec of borrows
        let mut total: i64 = 0;
        let mut cur = self.root.as_deref();
        loop {
            loop {
                match cur {
                    Some(n) => { stack.push(n); cur = n.left.as_deref(); },
                    None => break,
                }
            }
            match stack.pop() {
                Some(n) => { total += n.val; cur = n.right.as_deref(); },
                None => break,
            }
        }
        total
    }

    // R2 and R3: a borrowing iterator with no named iterator type. The
    // closure's compiler-generated type carries the borrow.
    fn iter(borrow self) -> impl Iterator(Item = borrow i64) {
        let mut stack: Vec(borrow Node) = Vec.new();
        let mut cur = self.root.as_deref();
        std.iter.from_fn(move || {
            loop {
                match cur {
                    Some(n) => { stack.push(n); cur = n.left.as_deref(); },
                    None => break,
                }
            }
            let n = stack.pop()?;
            cur = n.right.as_deref();
            Some(borrow n.val)
        })
    }
}
```

The twin writes `insert` with `while let`. The `loop`/`match` form above is
checked separately in `twins/bst_cursor.rs`, and rustc accepts both.

### The lexer and the parser: where R3 bites

The ruelex lexer already owns its source and hands out spans, so the strict
port keeps that shape (`twins/lexer.rs`, module `strict`):

```rue
pub struct Lexer {
    src: String,                 // owned: R3 rules out `src: borrow str`
    pos: usize,

    fn new(src: String) -> Self { Self { src: src, pos: 0 } }
    fn text(borrow self, t: Token) -> borrow str { self.src[t.start..t.end] }  // R2: from self
}
Lexer is std.iter.Iterator;      // Item = Token, a span: no borrow, no lifetime
```

The zero-copy shape most Rust code would write needs a type that holds a
borrow. Under the refined rule it is a borrowing type (module `refined`):

```rue
pub borrow struct Lexer {        // one implicit lifetime, from its borrow field
    src: borrow str,
    pos: usize,
}
pub borrow struct Token {
    kind: Kind,
    text: borrow str,            // the same implicit lifetime as the Lexer it came from
}
pub fn lex(src: borrow str) -> Lexer { Lexer { src: src, pos: 0 } }  // elision: one input
```

This lowers to Rust's `Lexer<'a>`, `Token<'a>` and `fn lex(src: &str) ->
Lexer<'_>`. Both modules produce identical token dumps.

JSON shows the same split. The strict parser (`twins/json.rs`, module
`strict`) uses free functions over an explicit `(src, &mut pos)` pair and
builds an owned tree. That is today's Rue style, and it works. Two smaller
lessons came out of it:

- **Elision shapes the API.** `get(obj, key) -> Option<&Json>` has two
  borrowed inputs, so elision cannot place the result. As a method,
  `fn get(borrow self, key: borrow str) -> Option(borrow Json)` it is fine.
  "Put the borrowed-from value in `self`" is ordinary Rust style.
- **Let the caller choose between two borrows.** A result that borrows from
  one of two inputs (`get_or(obj, fallback, key)`) is C4. The strict port
  writes it as `v.get(key).unwrap_or(borrow fallback)` instead. The refined
  port keeps the two-parameter form, as
  `-> borrow(obj, fallback) Json`.

Implementing a std trait needs no lifetime at all:

```rue
Json is std.fmt.Display;
// a member of Json:
fn fmt(borrow self, inout f: std.fmt.Formatter) -> std.fmt.Result { ... }
```

### join_inout

`std/parallel.rue`'s `join_inout` wraps the `@join_inout` compiler intrinsic
today. Over Rust's std it is an ordinary function (`twins/parallel.rs`):

```rue
pub fn join_inout(comptime A: Send, comptime B: Send,
                  inout left: A, inout right: B,
                  left_fn: fn(inout A), right_fn: fn(inout B)) {
    std.thread.scope(|s| {
        s.spawn(|| left_fn(inout left));
        right_fn(inout right);
    });
}
```

`thread::scope`'s own signature names `'env` and `'scope`, a C4 shape Rue
could not declare, but calling it names nothing. The twin also splits one
slice into `chunks_mut` pieces for scoped threads, and holds a `MutexGuard`
in a local. Neither has a spelling in today's `std.parallel`. `Send` stands
in for ADR-0098's "transferable".

### A library type: the ordered map

The survey says the rule bites on types and impls, not calls, so
`twins/ordered_map.rs` implements a full std-style collection: an
insertion-ordered map, the type wordfreq and the pool files approximate.

```rue
fn get(borrow self, k: borrow K) -> Option(borrow V)                       // ok, C2
fn get_or_insert_with(inout self, k: K, f: impl FnOnce() -> V) -> inout V  // ok, replaces Entry
fn iter(borrow self) -> impl Iterator(Item = (borrow K, borrow V))         // ok, opaque
fn values_mut(inout self) -> impl Iterator(Item = inout V)                 // ok
fn retain(inout self, keep: impl FnMut(borrow K, inout V) -> bool)         // ok, C1
fn drain(inout self) -> impl Iterator(Item = (K, V))                       // ok, borrows inout self

OrderedMap(K, V) is std.ops.Index(borrow str);      // ok: Rust accepts header elision here
OrderedMap(K, V) is std.iter.Extend((K, V));        // ok
OrderedMap(K, V) is std.iter.FromIterator((K, V));  // ok
OrderedMap(K, V) is std.iter.IntoIterator;          // ok: by value
OrderedMap(K, V) is std.fmt.Debug;                  // ok: uses DebugMap as a temporary
borrow OrderedMap(K, V) is std.iter.IntoIterator;   // needs the refined rule
```

The last line is what makes `for (k, v) in &map` work. Rust rejects the
elided form: the error says the lifetime in `type Item = &u64` "must come
from the implemented type". `IntoIter` must also be a nameable type, so the
impl needs both impl-header elision and a borrowing type.

## The survey

### Functions

| Class | Meaning | Distinct | Share | Raw |
| --- | --- | ---: | ---: | ---: |
| C0 | no borrow | 957 | 42.8% | 2,730 |
| C1 | borrows in inputs only | 692 | 31.0% | 891 |
| C2 | output borrows; elision produces exactly this signature | 413 | 18.5% | 472 |
| C3 | method on a type with a lifetime parameter | 129 | 5.8% | 147 |
| C4 | needs a named lifetime | 44 | 2.0% | 56 |

C0 through C2 is 2,062 signatures, or 92.3%. Of those, 29 use `'static`
(`thread::spawn`'s bounds, `&'static str` results), so Rue needs a spelling
for "owns no borrows". The same 2,062 also need other features:

| Feature | Signatures |
| --- | ---: |
| raw pointers | 135 |
| closure (`Fn*`) bounds | 126 |
| `impl Trait` | 21 |
| higher-ranked bounds | 13 |
| `dyn` | 6 |

The 44 C4 signatures fall into five shapes. Some have more than one.

| Shape | Count | Examples |
| --- | ---: | --- |
| output lifetime not tied to any input | 18 | 13 `unsafe` raw-pointer conversions (`slice::from_raw_parts`, `*const T::as_ref`, `CStr::from_ptr`) and 5 `leak` methods (`Vec::leak`, `String::leak`) |
| output tied to an input other than `self` | 12 | `Condvar::wait(&self, guard) -> guard`, `<[T]>::split_off_first` |
| lifetime used in a bound | 10 | `binary_search_by_key`, `is_sorted_by_key`, `thread::scope`, `Vec::leak` |
| one lifetime shared by several inputs | 9 | `HashSet::union`, `intersection`, `difference` |
| output tied to one of several inputs, no `self` | 1 | `Builder::spawn_scoped` |

### Types

146 of std's 406 stable public structs, enums and unions (36.0%) have
lifetime parameters:

| Kind | Count |
| --- | ---: |
| iterators and cursors | 101 |
| `fmt` types | 10 |
| borrowed handles and contexts (`IoSlice`, `BorrowedFd`, `Location`, `task::Context`) | 10 |
| guards and locks | 8 |
| entries and `PeekMut` | 7 |
| scoped threads | 2 |
| `Cow` | 1 |
| other | 7 |

140 have exactly one lifetime parameter. The six with two are the five
`fmt` Debug builders and `thread::Scope`.

There are 147 inherent methods on these types. Under the refined rule they
fall into these groups:

| Group | Count | Example |
| --- | ---: | --- |
| plain elision from `&self` or `&mut self` | 57 (39%) | `Formatter::write_str` |
| by-value `self` passes the type's lifetime to the result | 26 (18%) | `Pin::get_ref`, `Entry::or_insert` |
| `&self` returns the type's own lifetime (Swift's "copy" dependency) | 14 (10%) | `Iter::as_slice`, `Chars::as_str` |
| receiverless constructors and adapters, mostly single-input elision | 10 (7%) | `IoSlice::new`, `Ref::map` |
| two-lifetime types, or methods with their own lifetime parameter | 40 (27%) | the `core::fmt` Debug builders, `Formatter::debug_*`, `thread::Scope::spawn` |

### Traits and impls

- **All 133 stable, non-auto traits can be implemented** by a type with no
  lifetime parameters, without naming a lifetime.
- **Of 9,578 trait impls in std, 89% name no lifetime and 9% are for types
  that carry one.** The remaining 1.9% are lifetime-free in `Self` but still
  need a named header lifetime:
  - `IntoIterator for &X` (29)
  - `TryFrom` for `&[T; N]` (17)
  - `Sum` and `Product` over `&T` (69, one per numeric type)
  - `Extend<&T>` (13)
  - `FromIterator<&T>` (8)
  - `From<&T>` (7)

  Impl-header elision (below) covers all of these except `TryFrom`, whose
  header uses one lifetime twice.

## The refined rule

The ports and the survey together suggest four rules. Rue source still never
names a lifetime, and each rule maps directly onto a Rust signature with
lifetime parameters, so the shared IR carries the explicit form:

1. **R1 unchanged.** Locals hold borrows, under intraprocedural region
   inference.
2. **Parameter-named dependencies where elision is ambiguous.**
   `-> borrow(a, b) T` means the result borrows from `a` and `b`, so each
   output lifetime is the intersection of the named parameters' lifetimes.
   `borrow(static)` covers `leak` and the raw-parts constructors (unsafe).
   This covers every C4 shape except lifetimes inside bounds. Swift's
   experimental `@lifetime` annotations, used in its standard library to
   return `Span` (SE-0446, SE-0456), take the same approach: dependencies
   name parameters, never lifetimes.
3. **Borrowing types.** A struct or enum may hold borrows if it is declared
   as `borrow struct` or `borrow enum`. Such a type has exactly one implicit
   lifetime, and its values obey the same rules as borrows. (These are not
   RUE-1013's view types, which are about partial borrows of fields.)
   Elision extends to borrowing types in two ways: a by-value receiver
   passes the type's lifetime to elided outputs, and `borrow(copy self)`
   returns that lifetime from a `&self` method.
   This covers 140 of std's 146 lifetime-carrying types and 73% of their
   methods. The rest belong to the two-lifetime types.
4. **Impl-header elision.** When an impl header contains exactly one borrow,
   elided borrows in associated types and method bounds take its lifetime.
   rustc does not do this for Rust source, but the lowering can write the
   named form. This fixes `for x in &map`, `Extend<&T>` and `Sum<&T>`.

Under these rules Rue can still call, but not declare, two shapes:

- lifetimes used inside closure bounds: `thread::scope`'s `'env`, and
  `binary_search_by_key`, whose key closure may return a borrow of the
  element;
- types with two lifetimes, all of them `core::fmt` builders and
  `thread::Scope`.

## Tensions with current decisions

- **ADR-0037 and the formal core's decision 4.** Both say loans are
  second-class and never outlive the call. R1 is a direct reversal. The
  ADR-0097 Lean core proves safety for the current discipline. R1 needs
  either new metatheory for region inference, or a decision to take Rust's
  model (and MiniRust's semantics) as given at that layer.
- **ADR-0062's expression-scoped accessors** become a special case of R1
  rather than the mechanism.
- **ADR-0035 byte strings.** The wordfreq twin works on `&[u8]` because its
  input need not be UTF-8. The lexer uses `&str`. Rust's split between `str`
  and `[u8]` handled both without friction.
- **The RUE-246 ruling** ([abstraction-design-survey.md](abstraction-design-survey.md))
  declined conformance bodies and default methods. Rust traits require
  bodies and supply defaults, and std's blanket impls (`impl<T: Display>
  ToString for T`) apply to Rue types whether Rue lets users write blanket
  impls or not.
- **ADR-0096.** Its second-class callbacks fit `join_inout`'s interface, but
  the wrapper's body needs closures that capture. So do five of the seven
  ports, and a sixth uses a closure that captures nothing.
- **ADR-0086.** Rue can drive `Display` and `Debug` impls without macros, but
  `{:?}` and friends need a builtin that builds `fmt::Arguments`.

## Next steps

1. Port one large example, preferably gazette (9,021 lines, with arenas, a
   pool, a template engine and a markdown renderer), to measure the rule at
   scale.
2. Prototype R1 in Rue's checker. The question is whether the access-point
   exclusivity analysis can track loans held in locals within a body, or
   whether it needs a Polonius-style rewrite.
3. If the refined rule survives step 1, write the ADR that decides borrowing types,
   parameter-named dependencies and impl-header elision together, because
   they share one lowering.

## Reproduction

Twins (needs rustc 1.92.0 or newer):

```bash
cd docs/notes/rust-interop-borrow-exercise/twins
for f in wordfreq lexer bst bst_cursor dijkstra json parallel ordered_map; do
  rustc --edition 2024 -O -o /tmp/$f $f.rs
done
python3 check.py *.rs
```

Survey (needs nightly's `rust-docs-json` component):

```bash
rustup toolchain install nightly --profile minimal --component rust-docs-json
J=$(rustc +nightly --print sysroot)/share/doc/rust/json
cd docs/notes/rust-interop-borrow-exercise/survey
python3 classify.py "$J" /tmp/std-survey.json
python3 report.py /tmp/std-survey.json > report.txt
python3 classify.py "$J" --borrowing-types > borrowing-types.txt
```

`report.txt` and `borrowing-types.txt` are the outputs this note quotes.
