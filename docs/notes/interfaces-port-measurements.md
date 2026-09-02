# Porting std and the examples to `--preview interfaces`: what fit, what did not, and what it costs

This note records the first dogfooding pass of the `interfaces` preview
(spec 6.7) over the standard library and the example programs: which of the
duplication the corpus census in
[abstraction-design-survey.md](abstraction-design-survey.md) §2 expected
interfaces to remove actually came out, the exact v1 limitation behind each
place it did not, and before/after compile-time measurements of the affected
programs on one compiler binary. It is a point-in-time measurement of a
debug-built compiler on a small host, not a performance gate.

## Setup

- Compiler: one debug-built binary (`./buck2 build //crates/rue:rue`, the
  default platform) at revision `83aa419` ("Exempt the trusted standard
  library from the interfaces preview gate"), used for every run. The port
  itself changes no compiler code, so the "before" and "after" sources see
  the same compiler; the only compiler change relative to the pre-port
  baseline `8ddd676` is the trusted-std exemption (spec 6.7:25), without
  which the "after" std cannot be used from a program compiled without the
  flag.
- "Before": the sources at `8ddd676` (a `git worktree` of that revision),
  compiled with `RUE_STD_PATH=<worktree>/std` and no preview flag.
- "After": the ported sources, compiled with the ported `std/`, once without
  the flag (every ported program except `examples/hashmap` needs none — the
  trusted standard library is exempt) and once with `--preview interfaces`,
  to isolate the flag's own cost (with the flag, every module in the import
  graph is scanned for skolem-check roots rather than only std).
- Host: 4 cores, 15 GiB, Linux x86-64, `-O0` pipeline, internal linker,
  runs made serially on an otherwise idle machine. This is a debug-built
  compiler: its absolute times are roughly 5x the release-built numbers in
  [examples/caldera/SCALING.md](../../examples/caldera/SCALING.md), so only
  the before/after ratios carry over.
- Per program: 3 runs per configuration; the table reports the median wall
  time of the compiler process, and — from the `--benchmark-json` run with
  the median wall time — the in-process `semantic_analysis` phase, the
  `body_analysis` and `body_closure_collection` passes inside it, the number
  of semantic bodies analyzed (`critical_path.semantic_bodies.count`, the
  count of function instances the semantic phase analyzed: one per
  specialization, skolem checks included), and peak memory. The
  `--benchmark-json` output exposes no skolem-check counter of its own (the
  `cfg.skolem_checks_filtered` work counter is not serialized), so the
  skolem-check count is stated from the source: it is the number of
  bounded, non-constructor functions in the import graph, which after the
  port is 9 in std (`less_at`, `qsort_range_by_order`, `sort_by_order`,
  `is_sorted_by_order`, `cmp.equals`, `cmp.compare`, `cmp.min_by_order`,
  `cmp.max_by_order`, `fmt.display`) plus any in the program (0 in every
  example here; `HashMap` and `OrderedHeap` are type constructors and get no
  check, spec 6.7:24).

## What was ported

Line counts are `git diff --numstat 8ddd676` over the named paths.

### Standard library (+618 / −276 lines; 7 files touched, 2 added)

| File | Change |
| --- | --- |
| `std/interfaces.rue` (new, 54 lines) | `Equatable { equals }`, `Hashable { hash }`, `Ordered { less }`, `Display { to_string }`, and the freestanding assertion `StrBuf is Equatable + Hashable + Ordered + Display;`. |
| `std/strbuf.rue` (+38) | The four inherent methods the assertion is verified against: `equals` (delegates to the existing `equals_borrowed`), `hash` (FNV-1a/64 through `std.hash`, which `strbuf.rue` now imports — the import cycle `strbuf ↔ hash` is legal, 10.2), `less` (byte-lexicographic), `to_string` (a copy). |
| `std/hashmap.rue` (new, 237 lines) | `HashMap(comptime K: Hashable + Equatable, comptime V: type)`: open addressing with a dense entry store (`keys: ArrayBuf(K)`, `vals`, cached `hashes`, a `live` flag) and a separate slot table. Keys are compared in place through `keys.get_ref(e).equals(borrow k)` and only ever enter or leave the store by move, so a non-`Copy` key such as `StrBuf` works; values keep `StrMap`'s `Copy` limit (`get` returns by copy). |
| `std/strmap.rue` (311 → 66 lines) | Now a wrapper over `HashMap(StrBuf, V)` with its public API unchanged (`new(filler)`, `len`, `is_empty`, `insert(borrow k, v)`, `get`, `contains`, `remove`); `insert` tries `HashMap.update` through the borrowed key and copies the key only when it is new. Its CLI case (`std_strmap`) and the three examples that retain a `StrMap` (`lattice`, `meridian`, `rill` before the port) pass unchanged. The pooled-bytes key store, its compaction, and the byte-wise `_key_eq` are gone. |
| `std/sort.rue` (+100) | `sort_by_order(comptime T: Ordered, inout xs)` (quicksort by `less`, comparisons through `get_ref`, exchanges through `ArrayBuf.swap`, no element copied) and `is_sorted_by_order`. The `<`-based `quicksort`/`insertion_sort`/`is_sorted`/`binary_search` stay. |
| `std/binary_heap.rue` (+103) | `OrderedHeap(comptime T: Ordered)`, a min-heap by `less` over owning or `Copy` elements; `BinaryHeap` and `PriorityQueue` stay. |
| `std/cmp.rue` (+43), `std/fmt.rue` (+10) | `cmp.equals`, `cmp.compare`, `cmp.min_by_order`, `cmp.max_by_order` over `Equatable`/`Ordered`; `fmt.display` over `Display`. |

`IntMap` is untouched: an integer type has no inherent methods, so it cannot
satisfy a method requirement, and a bounded parameter has no `==`
(spec 6.7:20). The same fact is why every `<`-based std generic keeps its
operator body beside the new bounded one — one body cannot serve both an
`i64` and a `StrBuf`.

### Examples (+83 / −153 lines; 6 files)

| Program | Change | Flag needed |
| --- | --- | --- |
| `examples/hashmap` | `map.rue` 130 → 59 lines: the hand-written i64→i64 open-addressing table becomes `HashMap(Key, i64)` with `struct Key is Equatable + Hashable { n: i64 }` — the wrapper-type route, since `i64` itself cannot conform. `main.rue` is unchanged and still exits 42. | yes (`[[automatic_example]] preview = "interfaces"`) |
| `examples/wordfreq` | Counts in `HashMap(StrBuf, u64)` directly; a present word is bumped through `update(borrow word, …)`, a new one is copied in as its key. Its CLI case passes byte-for-byte. | no |
| `examples/harbor/pool.rue` | The linear `find` scan (compare every pooled string) becomes a `HashMap(StrBuf, u64)` index; `intern`/`find` are amortized O(length). The pool keeps its own byte store for the id→bytes direction. | no |
| `examples/ruelex/interner.rue` | Same: the linear scan and its private `entry_eq` become a `HashMap(StrBuf, u64)` index; symbol numbering (order of first appearance) is unchanged and the `--emit tokens` CLI case still matches byte-for-byte. | no |
| `examples/rill/pool.rue` | `StrMap(u64)` → `HashMap(StrBuf, u64)` (two lines). | no |

`lattice` and `meridian` were deliberately left as written so they serve as
controls: their `pool.rue` still names `StrMap(u64)`, which now IS
`HashMap(StrBuf, u64)` underneath, so their "after" numbers measure the
std-side cost of the feature with zero change to program source.

The harness gained `preview = "<feature>"` on an `[[automatic_example]]`
entry (and made its `contract` optional, so an entry can exist only to carry
the flag); no `BUCK` `rue_program` target needed `preview_features`, because
no staged program's own sources use interface syntax.

## What could not be ported, and the exact limitation

| Candidate (census §2) | Why not, in v1 terms |
| --- | --- |
| The comparator sorts: `gazette/site.rue` `sort_pages`/`sort_subsections` (via `page_before`), `lattice/query.rue:87` (`higher_priority`), `meridian/engine.rue:37` | Every one of them orders **ids** (`u64` indices into an arena) by data that lives outside the element: `page_before(site, eng, a, b, kind)` reads two `Page` records and a string pool and takes a sort-kind parameter; `higher_priority(workflow, left, right)` reads two tasks. An `Ordered.less(borrow self, borrow other)` receives only the two elements — there is no way to pass context, no closure, no function value, and `u64` cannot conform anyway. Making the elements self-describing structs would mean copying the compared fields (and, for gazette, the compared *strings*) into every element, which is the parallel-array pressure the census attributes to the element-access gap, not to interfaces. `meridian/engine.rue:37` sorts plain `u64`s and is not a comparator sort at all; it could use `std.sort.insertion_sort(u64, …)` today, without interfaces. |
| `gazette/pool.rue`, `mosaic/pool.rue` (the two interners with their own open-addressed index) | Both were written to hold each string's bytes exactly once and index over the pool's own arrays; a `HashMap(StrBuf, u64)` index keeps a second copy of every key. The no-copy version is a map keyed by pool **id** whose `hash`/`equals` read through the pool — a context-dependent key, which v1 cannot express (same limitation as the comparator sorts). `harbor` and `ruelex` had no index at all (linear scans), so for them the second copy buys an asymptotic improvement and was taken. |
| `examples/hashmap`'s `i64` keys, `IntMap`, the `<`-based `std.sort`/`BinaryHeap`/`PriorityQueue`/`cmp.min` | Primitive types cannot conform (no inherent methods, spec 6.7:10) and skolems have no operators (6.7:20): a bound can never stand in for `<` or `==`. The example was ported through a one-field wrapper struct; the std entry points were duplicated (bounded beside operator-based) rather than replaced. |
| `examples/tinydb`, `examples/jsonfmt` | No genuine fit. `tinydb`'s only comparisons are enum `==` on `Dept` inside one predicate function; `jsonfmt` renders through `std.json.to_string(value)`, which takes the value by move, so a `Display` conformance (`to_string(borrow self)`) would need a signature change in `std.json` for one call site. Neither has duplication a bound removes. |
| `File`/`TcpStream` `read`/`write`/`write_all` (227 lines, census §2) | Out of this pass's scope by the maintainer's list, and in any case an interface with `read(inout self, inout buf: ArrayBuf(u8)) -> Result(u64, E)` needs the two types to agree on `E` — they return different error enums — so it needs either an associated error type with a bound on it (v1 has associated types but no bounds on them) or a shared error type. |
| Any generic over a **hash-and-equal integer key** (`IntMap`, `examples/hashmap` without a wrapper) | Same primitive-conformance limit. A retroactive `i64 is Hashable;` is syntactically allowed (6.7:9) but can never verify. |

Nothing here needed a `where` clause or a bound on an associated type; those
limits were not reached because the port stopped at the operator and
context limits first.

## Measurements

All times are one debug-built compiler on the 4-core host described above.
"Semantic phase" is `phase_accounting.phase_ns.semantic_analysis`;
`body_analysis` and `body_closure_collection` are the two largest passes
inside it (inclusive spans, so they overlap each other and the phase).
"Semantic bodies" is the number of function instances the semantic phase
analyzed. `stdonly` is a two-line program, `const std = @import("std");
fn main() -> i32 { 0 }`, added to isolate what merely importing the ported
std costs. `welcome`, `fibonacci`, and `stdonly` are 5 runs; every other
program is 3. `hashmap` has no "after, no flag" row because the ported
example declares its own conformance and needs the flag.

| Program | Config | Wall (median) | Semantic phase | body_analysis | body_closure_collection | Semantic bodies | Peak memory |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| welcome | before | 0.05 s | 0.01 s | 0.00 s | 0.01 s | 1 | 53 MB |
| welcome | after, no flag | 0.09 s | 0.01 s | 0.00 s | 0.01 s | 1 | 53 MB |
| welcome | after, --preview interfaces | 0.05 s | 0.01 s | 0.00 s | 0.01 s | 1 | 53 MB |
| fibonacci | before | 0.06 s | 0.02 s | 0.01 s | 0.02 s | 2 | 59 MB |
| fibonacci | after, no flag | 0.06 s | 0.02 s | 0.01 s | 0.01 s | 2 | 58 MB |
| fibonacci | after, --preview interfaces | 0.07 s | 0.02 s | 0.01 s | 0.02 s | 2 | 58 MB |
| stdonly | before | 0.81 s | 0.40 s | 0.01 s | 0.03 s | 2 | 78 MB |
| stdonly | after, no flag | 0.85 s | 0.47 s | 0.11 s | 0.07 s | 11 | 81 MB |
| stdonly | after, --preview interfaces | 0.82 s | 0.41 s | 0.11 s | 0.08 s | 11 | 84 MB |
| hashmap | before | 0.96 s | 0.51 s | 0.14 s | 0.13 s | 27 | 83 MB |
| hashmap | after, --preview interfaces | 1.38 s | 0.82 s | 0.59 s | 0.41 s | 95 | 86 MB |
| wordfreq | before | 1.57 s | 0.99 s | 0.63 s | 0.60 s | 111 | 92 MB |
| wordfreq | after, no flag | 1.88 s | 1.23 s | 0.84 s | 0.78 s | 153 | 93 MB |
| wordfreq | after, --preview interfaces | 1.86 s | 1.19 s | 0.90 s | 0.78 s | 153 | 95 MB |
| ruelex | before | 2.88 s | 1.82 s | 1.40 s | 1.24 s | 219 | 122 MB |
| ruelex | after, no flag | 3.55 s | 2.36 s | 1.97 s | 1.73 s | 283 | 130 MB |
| ruelex | after, --preview interfaces | 3.50 s | 2.34 s | 1.88 s | 1.68 s | 283 | 130 MB |
| rill | before | 5.37 s | 3.58 s | 2.73 s | 2.85 s | 407 | 165 MB |
| rill | after, no flag | 5.29 s | 3.61 s | 2.89 s | 2.88 s | 446 | 160 MB |
| rill | after, --preview interfaces | 5.75 s | 3.93 s | 3.29 s | 3.18 s | 446 | 165 MB |
| harbor | before | 23.16 s | 18.60 s | 10.80 s | 17.09 s | 1381 | 288 MB |
| harbor | after, no flag | 22.77 s | 18.33 s | 10.72 s | 16.72 s | 1411 | 296 MB |
| harbor | after, --preview interfaces | 21.66 s | 16.93 s | 10.00 s | 15.48 s | 1411 | 295 MB |
| lattice | before | 26.09 s | 20.22 s | 10.13 s | 18.51 s | 1267 | 319 MB |
| lattice | after, no flag | 27.36 s | 21.20 s | 10.96 s | 19.50 s | 1298 | 329 MB |
| lattice | after, --preview interfaces | 26.50 s | 21.13 s | 10.58 s | 19.44 s | 1298 | 331 MB |
| meridian | before | 120.69 s | 102.31 s | 34.68 s | 98.21 s | 3126 | 891 MB |
| meridian | after, no flag | 118.66 s | 101.07 s | 34.33 s | 97.07 s | 3171 | 906 MB |
| meridian | after, --preview interfaces | 116.75 s | 98.24 s | 33.99 s | 94.37 s | 3171 | 902 MB |

Every individual run, so the spread is visible (semantic phase in ms, in
run order):

| Program | Before wall runs (s) | After wall runs (s) | After+flag wall runs (s) | Semantic ms, all runs (before / after / after+flag) |
| --- | --- | --- | --- | --- |
| welcome | 0.05, 0.05, 0.05, 0.05, 0.05 | 0.05, 0.09, 0.09, 0.09, 0.06 | 0.05, 0.05, 0.05, 0.05, 0.05 | 9/9/9/9/9 · 9/9/9/9/9 · 9/11/10/9/9 |
| fibonacci | 0.07, 0.07, 0.06, 0.06, 0.06 | 0.06, 0.06, 0.06, 0.06, 0.11 | 0.07, 0.07, 0.06, 0.08, 0.06 | 19/21/17/18/18 · 17/17/18/18/30 · 19/19/19/25/18 |
| stdonly | 1.03, 0.85, 0.78, 0.81, 0.73 | 0.82, 0.91, 1.00, 0.82, 0.85 | 0.80, 0.83, 0.91, 0.81, 0.82 | 432/368/384/403/347 · 435/506/569/426/466 · 415/432/497/428/413 |
| hashmap | 1.00, 0.89, 0.96 | — | 1.50, 1.38, 1.30 | 557/469/511 · — · 886/816/794 |
| wordfreq | 1.35, 1.57, 1.68 | 2.09, 1.77, 1.88 | 1.85, 2.03, 1.86 | 802/990/1067 · 1350/1123/1232 · 1200/1328/1190 |
| ruelex | 3.11, 2.76, 2.88 | 3.40, 3.55, 3.61 | 3.50, 3.32, 3.59 | 2056/1686/1817 · 2229/2355/2380 · 2335/2172/2297 |
| rill | 5.37, 5.40, 4.95 | 5.29, 5.25, 5.47 | 5.86, 5.75, 5.38 | 3579/3576/3303 · 3607/3561/3714 · 3967/3931/3558 |
| harbor | 22.26, 24.28, 23.16 | 24.51, 20.68, 22.77 | 20.48, 21.66, 22.16 | 17216/19366/18598 · 19680/16130/18325 · 16155/16927/17555 |
| lattice | 25.67, 26.09, 26.78 | 28.35, 26.88, 27.36 | 26.46, 26.50, 26.79 | 19859/20217/20974 · 22166/20949/21203 · 20593/21130/21357 |
| meridian | 119.45, 120.69, 120.70 | 118.66, 120.37, 117.33 | 127.14, 116.31, 116.75 | 101441/102306/102696 · 101070/102169/97601 · 107149/98817/98236 |


Work counters from the median run (functions in the import graph is
`source_metrics.functions`; materializations are `semantic_provider`
counters):

| Program | Config | Functions in graph | Semantic bodies | Function materializations | Nominal materializations |
| --- | --- | ---: | ---: | ---: | ---: |
| stdonly | before / after | 1 / 1 | 2 / 11 | 2 / 13 | 1 / 6 |
| hashmap | before / after+flag | 22 / 72 | 27 / 95 | 5 / 17 | 2 / 8 |
| wordfreq | before / after | 99 / 127 | 111 / 153 | 40 / 45 | 6 / 12 |
| ruelex | before / after | 203 / 252 | 219 / 283 | 43 / 61 | 11 / 18 |
| rill | before / after | 377 / 402 | 407 / 446 | 80 / 85 | 34 / 39 |
| harbor | before / after | 1233 / 1251 | 1381 / 1411 | 588 / 594 | 83 / 89 |
| lattice | before / after | 1225 / 1244 | 1267 / 1298 | 1000 / 1006 | 61 / 67 |
| meridian | before / after | 3078 / 3108 | 3126 / 3171 | 2823 / 2828 | 38 / 44 |

## Interpretation

**The feature's cost is visible only where the port added instantiations;
the checks themselves are a small constant.** The cleanest isolation is
`stdonly`: importing the ported std and calling nothing analyzes 11 bodies
instead of 2. The 9 extra bodies are exactly the 9 skolem checks (spec
6.7:19 makes every bounded std function a root of every program's body
closure, called or not), and they cost about 60 ms of semantic phase here
(median 403 → 467 ms; the run ranges 348–432 vs 427–569 ms do not overlap)
plus 5 nominal materializations (the `ArrayBuf(T)` instantiated at the
skolem of `sort_by_order` and friends). That is 0.3% of `harbor`'s or
0.06% of `meridian`'s compile, and it does not grow with the program.
`welcome` and `fibonacci`, which import nothing, are unchanged to the
millisecond in the semantic phase (their wall-time medians move by noise;
`welcome`'s 0.05 → 0.09 s "after, no flag" median is three 0.09 s runs
against a 9 ms semantic phase in every run).

**`--preview interfaces` itself costs nothing measurable.** With the flag,
skolem roots are collected from every module rather than only std; no
example declares a bound of its own, so the body count is identical with
and without it on every program, and the wall and semantic-phase
differences between "after" and "after, --preview interfaces" are inside
the run spread in both directions (harbor −1.1 s, rill +0.5 s,
meridian −1.9 s, lattice −0.9 s).

**Where the cost is real, it is instantiation, not verification.** The
three programs whose semantic phase grew by more than noise are the ones
that now instantiate `HashMap(StrBuf, u64)` where they previously had a
hand-written map or a linear scan: `wordfreq` +24% semantic (111 → 153
bodies), `ruelex` +30% (219 → 283), `hashmap` +60% (27 → 95, the
`i64`→`i64` table replaced by `HashMap(Key, i64)` plus its four
`ArrayBuf` instantiations). The added bodies are ordinary specializations
— `HashMap`'s nine methods, `ArrayBuf(K)`/`ArrayBuf(V)`/`ArrayBuf(bool)`
/`ArrayBuf(u64)`, `Option(K)`, `StrBuf.equals`/`hash` — the same price a
hand-written generic map would have paid; conformance verification of
`StrBuf is …` is one cached comparison per (type, interface) per body and
does not show up as a pass of its own. `rill` (whose `StrMap` was already
a hash map) grows by 39 bodies and 0.8% semantic time; `harbor`, `lattice`,
and `meridian` grow by 30–45 bodies (the 9 skolem checks, `HashMap`'s
methods under `StrMap`, and the `StrBuf` conformance methods) and their
wall times move by −1.7%, +4.9%, and −1.7%, all inside their own run
spreads (lattice's three "after" runs are 26.9–28.4 s against 25.7–26.8 s
before; harbor's are 20.7–24.5 s against 22.3–24.3 s).

**What the port bought.** On the compile side, nothing: the feature
removed 245 lines of `StrMap` and 71 lines of `examples/hashmap`, and
added 618 lines of std, most of it new capability (`HashMap` for any
conforming key, `sort_by_order`, `OrderedHeap`) rather than removed
duplication. On the program side, `harbor`'s and `ruelex`'s interning went
from a scan of every pooled string to a hash probe. The census's
"about 1,300 lines" for interfaces assumed one `HashMap`, one interner,
`Read`/`Write`, and one `Hash`; this pass delivered the `HashMap` and the
interner index, and found that the other interner and comparator
candidates are blocked by context-free requirement signatures rather than
by anything a v1.1 with `where` clauses or associated-type bounds would
fix.

## Notes on the feature in use

- **The trusted-std exemption is what makes the port usable.** Before it,
  every program touching `StrMap` — every `lattice`/`meridian`/`rill` build,
  every `std_strmap` case — would have needed `--preview interfaces` the
  moment `StrMap` wrapped a bounded `HashMap`. With it, only
  `examples/hashmap` (which declares its own conformance) needs the flag,
  and the harness change is one TOML key.
- **Bounds compose with `get_ref` cleanly.** `keys.get_ref(e).equals(borrow k)`
  and `xs.get_ref(i).less(borrow xs.get_ref(j))` both analyze as place
  chains: a requirement call on a borrowed element, with a second borrowed
  element as the argument, needs no temporaries and copies nothing. This is
  the pairing the census's §9.3 asked for and it works today, from user code
  as well as std.
- **Constructors are checked late, functions early.** `HashMap`'s body is a
  type constructor (spec 6.7:24), so a member the bound does not provide is
  only reported when the map is instantiated — while a mistake in
  `sort_by_order` is reported at the definition by the skolem check, with the
  `while checking … against the bound of parameter T` note. Both behaved as
  specified; the asymmetry is noticeable when writing std, because the
  constructor is where most of the generic code lives.
- **The E0305 help text is wrong for a user without the flag.** A program
  compiled without `--preview interfaces` that passes a non-conforming type
  to `std.cmp.equals` is told to `add \`Val is Equatable;\``, which is itself
  gated (6.7:25): the help should mention the flag. Reproducer:
  `struct Val { n: i64 } fn main() -> i32 { let a = Val { n: 1 }; if std.cmp.equals(Val, borrow a, borrow a) { 0 } else { 1 } }`
  without the flag (the `std_bound_is_still_checked_without_the_preview`
  spec case pins the E0305 itself, not the help).
- **`ArrayBuf.swap` is documented COPY-`T` but is a move-swap.** Its body
  reads both cells by `read_at` and writes each back once, so ownership is
  preserved; `sort_by_order` and `OrderedHeap` rely on that for `StrBuf`
  elements and the byte-copy is never duplicated. The comment on `swap`
  overstates the limit and could be relaxed — that is an `ArrayBuf`
  documentation issue, not an interfaces one.
- **A method named like an imported module shadows nothing but reads badly.**
  `StrBuf.hash` had to call `std.hash.fnv1a_strbuf`, and a module binding
  `const hash = @import("hash.rue")` next to `fn hash(borrow self)` is legal
  (different namespaces) but confusing; `strbuf.rue` binds it as `hashing`.
- **No bug in stages 1–3 was hit** across the std port, the five example
  ports, the trusted-std exemption, and the spec/CLI/UI suites: every
  diagnostic the port provoked (E0302 for a missing member, E0305 at a call,
  E1100 for a user assertion, E0411 under a skolem check) was the specified
  one at the specified site.
