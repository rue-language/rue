import RueCore.Corpus

/-!
# RueCore.Gen — a seeded generator of fragment programs (RUE-2229)

xref: examples

The bridge corpus (`Corpus.lean`) is hand-written, so it only exercises the
shapes its authors thought of. This module generates fragment programs from a
seed, so the differential bridge (ADR-0097, decision 4) can also run programs
nobody wrote: the verification-guided-development loop of generate, run the
model and the implementations, compare. `lake exe ruecore-corpus --gen N
--seed S` appends `N` generated cases to the corpus JSON in the schema
`Corpus.lean` documents.

## What a generated program is guaranteed to be

Generation is type-directed under a scope of binders, so every program is
closed, well-scoped, and simply typed by construction: every `use p` and
`drop p` names a place rooted at a binder in scope, operator operands are
`int`, both arms of an `if` have the wanted type, `assign p e` targets a place
rooted at a `mut` binder and `e` has the place's type, and every literal is in
bounds (`InBounds`). A program's struct **and enum** declarations are drawn
first and are well-formed by construction (§3's class equation and `6.3:19`'s
payload join, `3.8:18`'s `@copy` restriction, and fields and payload
components naming only declarations drawn earlier, which is `3.0:5`'s
acyclicity); a struct literal supplies one initializer per declared field, an
enum literal one argument per declared payload component of the variant its
tag names (`6.3:16`), an array literal one element per unit of its length and
a repeat form only at a `Copy` element (`7.1:38`), a `match` has **exactly one
arm per variant in declaration order** typed under that variant's payload
locals, a projection names a declared slot at the type it is asked for, a
constant index is within its array's length (`7.1:9`), and a read below a
dynamic index is at a `Copy` leaf, a `loop` body is `unit`-typed, a
`break` stands only inside a loop body and only as the **last** form of its
block (below, "Loops"), and a `return` or `@panic` only where a break arm could
stand or as the function body's last form, never in an operand (below, "Return
and panic arms"). So `Print.tyOf` succeeds on every generated
program, and whatever the verified checker rejects, it rejects for an
ownership reason — a use after move, a use of a partially moved value, a
linear leak, a linear discard or overwrite, a disagreeing join, a move out of
a destructor-bearing value, a destructure whose residue carries a linear
value, a write into an array with a moved-out element, a dynamic index into
one, a dynamic index under a declared-`linear` struct, a value moved by one
loop turn and used by the next, loop exits that disagree on a linear value, a
`break` past a live linear local — which is what the bridge's refusal table
covers.

Exhaustiveness is a property of the *draw* rather than a premise the draw
might miss: `expr` builds the arm list by mapping over the declaration's own
variant list, so the arm count and the arm order are the declaration's by
construction and (Match) §5.5's `arms.length = ed.variants.length` cannot
fail. Every arm is drawn at the `match`'s own type except a **break arm**
(`breakArm`, below) or a **return or panic arm** (`divArm`), which is
`never`-typed; `firstArmTy` (`Checker.lean`) skips a `never` arm and at most
one arm diverges, so the type it fixes is always a drawn arm's and its choice
is never the reason a generated case is refused.

## How deep a place goes

A **use** and a **`@drop`** are drawn at a path of one *or two* field steps —
a field of a field — wherever the declarations reach that far and the place
rules admit the path (`paths2`, `pathOk`, which is `projSlots`' legality test
read at a whole path). Depth 2 is where the path machinery actually recurses:
`OwnSt.get` and `setAt`'s padding, `ContentsMatches.readAt`/`.writeAt`, and
§6.11's nested `⊘`-skip. One seed case (`deep_path`) is not coverage of it.

An **assignment target** is at most **one** field step deep, and that is a
deliberate exception: a reinitialising assignment at depth ≥ 2 is a compiler
defect (RUE-2319) — it runs the overwrite-drop on the already moved-out
position and then leaks the value it stored — so the model and the compiler
disagree there for a reason that is not the model's, and a generated case with
the shape would be a false bridge failure rather than a finding. Depth-2 uses
and drops are unaffected by it: the defect is in the assignment path, and
depth-2 use, `@drop` and assignment-under-a-live-value were all checked by
hand against the compiler.

A step is a field slot or a **constant index** alike (`fieldSlots`), so a
depth-2 place may be `a[c].f`, `h.arr[c]` or `a[c][c']` as well as `h.f.g`.

A path through a **declared-`linear`** struct is drawn exactly as any other
(RUE-2339). `pathOk` does not ask which of §4.2's plans a path selects, so
where a proper prefix of it is a struct declared `linear` the checker takes the
`Declared(d, π_s)` plan (`declaredPrefix`, `Syntax.lean`) and
(Use-Declared-Linear-Destructure) §5.1, or the `@drop` read the same way,
decides the case — residue check included. (The one plan-dependent test the
draw makes is `3.8:68`'s root rule for an element move, which is the ordinary
plan's premise and not the destructure's; `pathOk` says why.) That puts a
destructure in 22 of the 200 programs at `--gen 200 --seed 7` (43 sites, 21 of
them two steps deep) and in 109 of the 1,000 at `--gen 1000 --seed 23` (195
sites, 107). The checker reaches a declared-plan rule in 11 and 52 of those
programs and accepts 2 and 6 of them; the deepest refusal is the
linear-residue premise, the E0474 of `3.8:60`, in 1 and 6, and a
declared-plan premise of any kind in 4 and 11. The rest are refused for the
reasons any generated case is — most often a linear value leaked at a scope
exit, a `match` arm or a discarded statement, or a use after the move —
because a declared-`linear` binder has to be consumed explicitly and a random
program seldom does.

One shape of it was a compiler-red case until RUE-2335 was fixed: a `@drop`
of a declared-`linear` place after a destructure strictly under it, which the
model accepts and the compiler rejected with E0406 (the seed case
`destructure_ancestor_dropped`). The draw does **not** avoid it, and did not
while it was red; that would have written the compiler's answer into the
generator. None of the 1,200 cases at the two
settings above has it, because it needs two nested declared-`linear` levels
and a `@drop` of the outer one after a use under the inner one, all rooted at
one binder. It is reachable all the same, and rarely: on the draws before
loops (RUE-2330), the first accepted case with it at seed 1 was
`gen_1_151382`, which the compiler refused with E0406 before RUE-2335, and it
was the only one
in the first 300,000. Drawing loops changed which program every setting
produces from its first loop draw on, so that name no longer holds that
program.

## Arrays (RUE-2331)

`[T; n]` is drawn as a field type and as a `let` binder type (`arrayOf`), over
the element types those draws already make, `n ≤ 3` with `0` among them. Its
values are literals and, at a `Copy` element, the repeat form `[e; n]`. Its
places are drawn two ways:

* **constant index** steps are steps of a place like field slots, so the
  existing use, `@drop` and assignment draws reach `a[c]` reads, element moves,
  element `@drop`s and one-step writes `a[c] = e`, and `h.arr[c]`,
  `a[c].f` and `a[c][c']` as depth-2 places;
* **dynamic index** forms (`dynPlaces`) are reads at a `Copy` leaf, writes and
  `Copy` `@drop`s, at the element and below it — `a[i].f`, `h.arr[i].f`,
  `a[i][j]`, `a[i].f[j]` — with at most two dynamic steps, each index bound by
  a `let` first (`bindIdx` says why) and out of bounds one draw in five.

The share of programs that reach each form, of 200 at `--gen 200 --seed 7` and
of 1,000 at `--gen 1000 --seed 23`: an array literal 100 and 453 (a zero-length
one 21 and 106), the repeat form 26 and 126; a constant-index `Copy` read 15 and
66, a non-`Copy` element or path-under-element move 7 and 31, a `@drop` at a
constant index 16 and 86, a constant-index write 7 and 16; a dynamic-index read
36 and 145, write 20 and 67, `@drop` 15 and 92 — below the element (`a[i].f`)
in 25 and 95, through a field (`h.arr[i]`) in 38 and 166, two dynamic steps
(`a[i][j]`) in 13 and 40. The bounds trap ends 19 and 71 runs, 11 and 36 of them
of programs the checker accepts.

The array refusals, as the deepest refusal of a rejected program at the same
two settings: `3.8:72`'s write into an array with a moved-out element (E0480)
2 and 4; `3.8:70`'s dynamic index into one 1 and none; §4.2's
`DeclaredLinearDynamic`, a dynamic index under a declared-`linear` struct
(E0904), 7 and 37; (Assign)'s linear overwrite at a dynamic-index write none
and 2.
Three array refusals are **not** drawn, each because the draw is typed to
avoid it rather than because ownership might: `3.8:68`'s element move out of a
non-root array (E0904, `pathOk`), the repeat form at a non-`Copy` element
(E0905, `atom`), and a read below a dynamic index at a non-`Copy` leaf (E0904,
`dynRead`). Nor are arrays drawn as an enum payload component or as a
program's result type, whose draws keep their weights; the seed corpus has
the shapes those would add.

Four generated shapes have disagreed with the compiler — three seeded, and a
fourth the return arms reached (RUE-2383) not yet — and none is drawn around — drawing around one would write the compiler's current
answer into the generator. A generated case with one of them is a bridge
disagreement to attribute to its issue by hand, as RUE-2335's was (above);
nothing in the tree counts them. On the current draws the acceptance settings,
`--gen 200 --seed 7` and `--gen 1000 --seed 23`, reach one of them, the
self-assignment, in four cases, and in every one the compiler stops first at
another error, so none of those **disagrees**. One case at those settings does
disagree, on a shape the return arms reached (RUE-2383, the last bullet
below): all 1,199 others agree with the compiler. At `--gen 200 --seed 41`
the self-assignment is unmasked in `gen_41_76` and `gen_41_112`, two programs
the return arms left unchanged.
Wider runs reach it unmasked: `gen_101_207` (`--gen 400 --seed 101`) was
drawn before the restoring statements below changed the draws, and is a
RUE-2346 disagreement. The other names below are the cases that reached each
shape on the draws before loops (RUE-2330), which changed the program every
setting produces from its first loop draw on. Wider runs at
other seeds have also reached two compiler defects filed from them, RUE-2347
(a CFG verification error on a `match` in one `if` arm) and RUE-2348 (an
internal error on a float array bound inside an enum-valued block). Any other
generated disagreement is a finding to file.

* `a[c] = a[c]` (seed `array_elem_self_assign`, RUE-2346, still red): the
  model refuses it, because (Assign) §5.2 runs the right-hand side first and
  the write then goes into an array with a moved-out element (`3.8:72`,
  `7.1:46`), while the compiler accepts it on purpose (RUE-228). On the current
  draws, three cases at `--gen 200 --seed 7` (`gen_7_145`, `gen_7_159`,
  `gen_7_181`) and one at `--gen 1000 --seed 23` (`gen_23_752`), each masked
  because the compiler first reports another error — an E0406 at seed 7, an
  E0205 in `gen_23_752`, which a return arm changed. Before loops, one
  case at seed 7 (`gen_7_101`, disagreeing) and three at seed 23
  (`gen_23_295`, `gen_23_868`, and `gen_23_652`, the last masked by an
  E0904).
* A dynamic index into a **zero-length array field**, `h.arr[i]` at
  `arr: [T; 0]` (seed `array_zero_length_field_dyn_read`): an internal
  compiler error in code generation where the model traps with `bounds`,
  until RUE-2345 was fixed; these agree now. Before loops, five cases at seed
  23 (`gen_23_108`, `112`, `126`, `636`, `718`).
* A place below a dynamic index after its field-reached array (or an ancestor
  of it) was moved (seed `array_dyn_write_after_field_move`): `gen_2_1694`
  (`--gen 1695 --seed 2`) reached it before loops, and it agrees now that
  RUE-2344 is fixed.
* `return { … }`, a `return` whose operand is a **block** (not yet seeded or
  filed): §2's `return e` and `4.9:2`'s `"return" expression?` admit a block
  operand, but the compiler's parser reads `return` before a `{` as a bare
  `return` — `return ()` (`4.9:3`) — and then the block as the next
  expression, so a function returning `i32` is refused with E0206. The printer
  writes a `let`-bound dynamic index as such a block (`bindIdx`), so a
  returned atom that reads below a dynamic index prints as one:
  `gen_23_444` (`--gen 1000 --seed 23`), which the model accepts and runs to
  `1`. `fn f() -> i32 { return { 2 } }` is the whole shape.

Calls are **not** generated yet: every generated case is a one-function
program (`Program.entry`), so the shapes RUE-2233 added that need a callee — a
frame unwound by an early `return` under a caller's live frame, a by-value
parameter dropped at a frame pop, recursion — are covered by the hand-written
seed cases only; an early `return` out of the entry function itself is drawn
(below, "Return and panic arms"). Generating calls needs a signature environment to draw callees from and a fuel bound that
recursion cannot escape; it is the follow-up this module's `expr` is shaped
for (`Corpus.Case` already holds a whole `Program`).

## Return and panic arms (RUE-2383)

`return` and `@panic` are drawn under the discipline a drawn `break` follows
(below, "Loops"): a diverging form is only ever the **last** form of an arm, a
branch has at most one diverging arm, and none stands in an operand, so no
syntax follows one (RUE-2376) and no `if` or `match` diverges as a whole. A
**return or panic arm** (`divArm`) is `return v` two times in three, `v` an
atom at the entry function's return type ((Return-Value) §5.7, (D-Return)
§6.9), and `@panic("gen")` otherwise ((Panic) §5.8, (D-Panic) §6.12) — bare
half the time, and otherwise the last form of a block after a `@drop` of a
non-`Copy` aggregate binder or a unit-typed leaf. It stands in four places:

* as a whole arm of an `if` — one `if` in eight on a random side, and one
  break arm in three turned into one (`maybeDiv`);
* as a whole arm of a `match` — one `match` in six on a random arm, and one
  break arm in three (`divArms`);
* as the arm of a loop's **exit statement** `if c { … } else { () }`, one in
  three (`drawLoop`), which with the break arms above is how a `return` or
  `@panic` reaches a loop body. A once-through loop's **tail** stays a break
  arm, because a loop every exit of which left the function would diverge as
  a whole and whatever followed it would be dead code;
* as the **function body's last form**, one body in ten: `let r = e; return r`.

`expr`'s `rt` flag says where one may stand: the function body, and the
statement positions of `let`, sequence, arm and loop-body draws under it. A
`let` initializer is one of those positions: an arm inside it may return or
panic, as a break arm there may, because nothing is pending at a `let`. An
operand, a struct, enum or array field, a call or intrinsic argument, a
condition, a scrutinee or an assigned value is drawn with `rt` off, which keeps `check`'s one incomplete shape (a `never` operator
operand, `Checker.lean`, "what completeness still costs") and RUE-2316's
pending-value edge — a value built for an earlier sibling, abandoned by the
jump — out of the generated corpus, as it does for `break`.

Every one of these draws reads a **side stream** (`side`), a second `StdGen`
split off the seed, and an arm it replaces is still drawn on the main stream
first and discarded. So a program with no diverging arm is exactly the program
the generator drew before RUE-2383, and one with a diverging arm differs from
its earlier draw only where an arm, or the body's tail, was replaced: 48 of the
200 programs at `--gen 200 --seed 7` and 275 of the 1,000 at `--gen 1000
--seed 23` differ. Of those, a `return` arm is in 25 and 137, a `@panic` in 10
and 76, a returning body tail in 19 and 92, a `return` or `@panic` inside a
loop body in 16 and 71; the checker accepts 25 and 127 of them, of which 2 and
19 end in a panic. The figures in the rest of this module were measured on the
draws before RUE-2383 unless they say otherwise; they hold as they stand for the
programs that did not change, and the headline acceptance figure below is
re-measured. The seed corpus keeps the hand-picked shapes
(`if_return_arm_affine`, `match_return_arm_linear`, `match_never_first_arm`,
`if_panic_arm_linear`, `panic_past_linear`, `ret_past_payload`).

## Loops (RUE-2330)

A generated loop is one of two shapes (`drawLoop`), and each **terminates by
construction**:

* **counted**, three loops in four: `let mut k: i64 = 0; loop { if k >= n {
  break } else { () }; k = k + 1; body }` (`countedLoop`), with `n ≤ 3`. The
  guard is the body's first statement, so the body runs at most `n` times
  whatever it does. The counter is in the body's scope as an **unmarked**
  binder — `Binder.mu` is the draw's mark, not the program's — so nothing the
  body draws writes it, although the printed `let` is `mut`;
* **once-through**: `loop { body; <break arm> }`, whose last form is a break
  arm (`breakArm`), so the body runs once and no turn reaches the back edge —
  RUE-1615's shape, where a move inside the loop is checked only against the
  exit.

Half the bodies open with an **exit statement**, `if c { <break arm> } else {
() }`, with a drawn condition. Two draws aim at the moves a random body seldom
gets accepted (`drawLoop`): half the counted bodies whose scope has a `mut`
struct or enum binder from outside the loop open with a **restoring
statement** (`restoreStmt`) — reassign-then-move `x = <fresh>; @drop(x)` at an
affine type, move-then-reassign `@drop(x); x = <fresh>` at a linear one — and
half the once-through loops whose scope has a linear struct or enum binder
drop it on **every** exit. Inside a loop body, one `if` in three and one
`match` in three has a break arm among its arms (`breakArmIdx`). A break arm is
`break`, `{ <leaf>; break }`, or — half the time where the scope holds a
non-`Copy` aggregate — `{ @drop(x); break }`: a move that meets an exit rather
than the back edge, which is RUE-1614's exit join when another exit keeps `x`.
Loops nest through their bodies, and a `break` targets the innermost loop, as
in §2. The draw is placed where it can see something to move: form 6 of `expr`
is a loop at `unit`, form 7 a loop statement before the rest at any type, both
weighted up where the scope holds a non-`Copy` aggregate (`ownedInScope`), and
at fuel 0 a loop statement with leaf body and condition precedes the leaf one
time in four where it does.

The body terminates because every loop inside it is one of these two shapes and
the fragment has no other back edge (no calls are drawn), so every generated
program terminates, and within the export fuel: every one of the 200 at
`--gen 200 --seed 7` and the 1,000 at `--gen 1000 --seed 23` completes and is
exported. The export enforces it: `lake exe ruecore-corpus --gen N` fails,
naming the cases, if a generated case does not complete at the export fuel,
rather than leaving it out as it does a seed case (`CorpusMain.lean`).

Two constraints are drawn around rather than left to chance, because each is a
question for the calculus rather than a finding the bridge should make:

* **no syntax after a diverging form** (RUE-2376): a `break` is only ever the
  last form of a break arm or of a once-through body, and a branch has at most
  one break arm — or one return or panic arm in its place (above, "Return and
  panic arms") — so no `if` or `match` diverges as a whole and nothing follows
  a `break` in its block — including as an operand followed by further
  operands, `S { x0: …, x1: break }`, which the compiler rejects with E0478
  and the checker accepts. `break` is also never drawn inside an operand, an
  argument or an initializer list at all, which keeps RUE-2316's pending-value
  edge (a value built for an earlier sibling, abandoned by the `break`) out of
  the generated corpus;
* **a `unit` body** (RUE-2379): every body is drawn at `unit`, because §5.7
  types a loop body there and the compiler also accepts a non-`unit` one.

The share of programs that reach each loop shape, of 200 at `--gen 200 --seed
7` and of 1,000 at `--gen 1000 --seed 23`: a loop 102 and 480 (172 and 775
loops), a counted one 86 and 389, a once-through one 35 and 174, a loop nested
in a loop's body 10 and 41; an `if` with a break arm inside a loop body — an
exit statement or a drawn arm — 66 and 301, a `match` with one 2 and 4; a move
or `@drop` of a non-`Copy` binder from outside the innermost loop, inside its
body, 49 and 236, of which the checker accepts 6 and 26 — inside a counted
loop, where it can meet the back edge, 39 and 182 (3 and 14 accepted), of a
linear value 20 and 95 (1 and none accepted), and beside an assignment to the
same binder in the same loop, the restoring idioms, 17 and 79 (2 and 16
accepted). The checker accepts 44 and 177 of the programs with a loop. The
loop refusals, as the deepest refusal of a rejected program: no loop-head
state — a value moved by one turn and used by the next, `3.8:79`'s E0205 — 11
and 49; the exits disagreeing on a linear binding (`3.8:80`, E0443) none and 6;
a `break` past a live linear loop-local (E0406) and the `⟨diverge, Σ_h⟩` leak
none at either, the second by construction, since no generated loop diverges.
An accepted linear move in a loop stays rare because the value then has to be
consumed after the loop as well, which a random rest of the program seldom
does.

## What it deliberately does not guarantee

Ownership. Moves, drops, assignments, `match` arms and scope exits are chosen
at random, so a large minority of the programs are rejected by the checker and
refused by the machine — 85 of 200 at `--gen 200 --seed 7` and 431 of 1,000 at
`--gen 1000 --seed 23` (87 and 438 before the return and panic arms, the
figures the weights below were tuned against). Both
are recorded (`Corpus.caseJson` reads them off `checkProgram` and `run` as for
any case), never filtered: a rejected program checks that the compiler rejects
it too, an accepted one that the three implementations agree with the
interpreter's trace. At those two settings 88 of 200 and 427 of 1,000 programs
contain a `match`.

Every figure in this module is a count over the programs `generate` returns at
the setting named, read off `checkProgram`, `Explain.deepestFailure` (a case's
"deepest refusal") and `run`; a figure "without" a weight is the same count
over a copy of this module with that one draw removed.

## Bias

The choices are weighted toward the shapes the safety theorems
(`Soundness.lean`) are about, all in one place (`expr`, `leaf`, `atom`) so
the weights can be read and changed:

* when a struct binder of the wanted type is in scope, using it (a move)
  is preferred to building a fresh value — including inside one arm of an
  `if`, which is how join disagreements arise;
* `let` binders are mostly structs and mostly `mut`, so linear values
  reach scope exit and assignments have targets;
* about half of the declarations not declared `linear` carry a destructor, so
  a drop is as often observable as not, and a declaration whose fields join to
  `Linear` is `Linear` whatever its attribute says (§3), which is how the
  linear-through-a-field shapes arise;
* about two declarations in seven are declared `linear`, which puts one in
  126 of the programs at `--gen 200 --seed 7` and 579 of 1,000 at
  `--gen 1000 --seed 23`, and a path through one is §4.2's declared-linear
  destructure (above, "How deep a place goes");
* a sequence's discarded statement is mostly unit-typed, where `assign`
  (whose right-hand side may use the target binder itself, so
  reinitialisation after a move and overwrite of a live value both arise)
  and `@drop` live; `@drop` prefers a binder §6.11 walks into — a struct, an
  enum or an array — but may name any binder, since the calculus allows `@drop`
  of a place of any class;
* integer literals are small, `0` among them, with an occasional `min_T` or
  `max_T` at the drawn type, so every arithmetic operator can trap — and the
  narrow types make that likely rather than rare;
* types are drawn from every width and both signednesses, `i64` a little more
  often than the rest, and an operator's operands share the drawn type
  (`4.2:1`, and `4.3a:9` for a shift's amount);
* `@intCast` draws its *source* type independently of its target, so most
  casts are between two different types and a good share of them trap;
* `@dbg` is drawn in unit position, where it competes with `@drop` and
  assignment, so a generated program's stdout usually interleaves the two
  observation channels;
* an enum's payload components are mostly **struct** types, because a struct
  is what carries a destructor and a declared attribute — so the payload
  classes span `Copy`, `Affine` and `Linear`, `6.3:19`'s join has something to
  say, and an enum one of whose variants carries a `linear` payload is itself
  `Linear` whichever variant a value holds;
* a `match` is weighted up sharply when the scope already holds a place of
  enum type, and its scrutinee is then that place rather than a fresh
  temporary: a projection first (`3.8:22`'s partial move at a field, whose
  sibling fields still drop at scope exit) and a binder next (the move that
  makes a second `match` on the same place the E0205 the compiler reports).
  That weight on its own starved the shape it is for, because the branch it
  fired on was the rarer one: with the weight alone only 24 of 149 `match` sites
  at `--gen 200 --seed 7` and 61 of 676 at `--gen 1000 --seed 23` have an enum
  place in scope at all. So where the scope offers no place of the drawn enum's
  type the draw **makes** one half the time — `let v = <the temporary> in
  match v`, sometimes with one statement in between — instead of matching a
  temporary, and the arms are then typed under a scope that still holds the
  consumed binding. Measured with it: 98 of 161 sites and 543 of 811 have a
  place in scope, 93 and 509 scrutinize one (the weight alone reaches 20 of 149
  and 45 of 676), and the E0205 a second `match` on the same place is — (Use-Move)
  §5.1's `fully-owned` premise at an enum type — becomes the *deepest* refusal
  of 17 of 200 and 93 of 1,000 cases, where the weight alone reaches 3 and 5.
  The n-way **join** conflict stays rare at either setting — no case in 200 at
  seed 7 and none in 1,000 at seed 23, while the binary (If) join is the deepest
  refusal of none and 1 —
  because it needs two arms to disagree about an entry that carries a
  linear value and outlives the `match`, which random arms seldom do;
* a `let` binder is biased toward a declaration that holds an enum in
  a field, which is what puts a projection of enum type in scope at all;
* one field type in five and one `let` binder type in five is an **array**
  (`arrayOf`), wrapped around the type the draw would have made anyway, so the
  other weights keep their meaning; one array in four is nested, and the length
  is small with `0` among them (`arrayLen`). At `--gen 200 --seed 7` 132 programs
  declare a struct with an array field and 106 contain an array literal or
  repeat form; at `--gen 1000 --seed 23`, 637 and 482;
* an index is a **constant** step of a place like a field slot (`fieldSlots`),
  so the use, `@drop` and assignment draws above reach `a[c]`, `a[c].f`,
  `h.arr[c]` and `a[c][c']` with no draw of their own, held to `3.8:68`'s root
  rule at a non-`Copy` leaf (`pathOk`) and to one step for an assignment
  (`assignSlots`);
* where the scope has an array to index, an **array statement** is weighted up
  (`arrayStmt`: form 5 of `expr`, weight 6, and at fuel 0 half of the leaves),
  because the type-directed draws alone reach an index form only where the
  type they want is the element's: without it, 30 of 200 and 138 of 1,000
  programs contain an index form, with it 66 and 294. The statement is a
  dynamic-index write, read or `Copy` `@drop` (`dynUnit`) or a constant-index
  read, `@drop`, write or element move, and an atom at a `Copy` type is a read
  below a dynamic index one time in three where the scope offers one
  (`dynRead`);
* a dynamic index is out of bounds one draw in five, and always at a
  zero-length array (`dynIdx`), and it is bound by a `let` first so that the
  printed program keeps it dynamic (`bindIdx`);
* an arm's body is an ordinary drawn expression over the payload locals, so
  what the arm does with a payload is whatever the existing weights do with
  any binder: move it into an outer `mut` binding or into a fresh aggregate,
  `@drop` it, read a `Copy` one twice, or leave it — which at an `Affine`
  payload is the drop `6.3:17` times at the arm's end and at a `Linear` one is
  the leak §5.6 rejects (E0406).

One draw is deliberately **narrower** than the type would allow, and it is not
a bias but an invariant. `@total_cmp`'s two operands are drawn as float
*literals* rather than as arbitrary float expressions, because `@total_cmp` is
the only form in the fragment that can observe a NaN's *sign*, and that sign is
`σ_NaN` — a target parameter (§2, `3.12:44`), negative on x86-64 and positive
on AArch64. An exported expectation that read one would be right on one target
and wrong on the other. `floatLiteral` draws only finite decimals, so no NaN
can reach a `@total_cmp` operand *by construction*; NaNs are still generated
freely everywhere else (a `0.0 / 0.0` draw does reach `@dbg`, which renders
`NaN` sign-blind, `3.12:42`). Widening this draw means giving the exporter a
`σ_NaN` that follows the target.

Programs are fuel-bounded: a fuel of two or three is drawn per program and
every compound form spends one unit on its operands, so the nesting a case
reaches is a few levels and it stays readable; shrinking is out of scope. A
loop spends fuel as any compound form does, so loops nest at most as deep as
the program's fuel, and each one terminates (above, "Loops").

## Determinism

The generator is a pure function of `(n, seed)` for the pinned toolchain:
`StdGen` from `Init` is threaded through a `StateM`, and nothing reads the
environment (a toolchain bump that changes `StdGen` changes every case, so
a finding filed from a generated run records the seed and quotes the
program). The first `i` cases of a run are the same for every `n > i`, so a
case named `gen_<seed>_<i>` can always be regenerated from its name alone.
-/

namespace RueCore.Gen

open Expr

/-- (helper) A binder in scope: its type and its `mut` mark. The list is
innermost first, as `Ctx` is, so a binder's position is its de Bruijn
index. -/
structure Binder where
  ty : Ty
  mu : Bool

/-- (helper) The binders in scope, innermost first. -/
abbrev Scope := List Binder

/-- (helper) The generation monad: two `StdGen`s threaded through. The first
is the **main** stream every draw reads; the second is the **side** stream
(`side`), which only the diverging-arm draws (RUE-2383) read, so adding them
left every other draw of every program where they do not fire unchanged. -/
abbrev G := StateM (StdGen × StdGen)

/-- (helper) A uniform natural number in `[lo, hi]`, off the current stream. -/
def nat (lo hi : Nat) : G Nat :=
  modifyGet fun (g, h) => let (n, g') := randNat g lo hi; (n, (g', h))

/-- (helper) A uniform boolean, off the current stream. -/
def bool : G Bool :=
  modifyGet fun (g, h) => let (b, g') := randBool g; (b, (g', h))

/-- (helper) Run a draw on the **side** stream: the two streams swap for its
duration, so whatever it draws leaves the main stream where it was. The
`return`/`@panic` draws (`divArm`, RUE-2383) run here, and where one replaces an
arm the arm it replaces is still drawn on the main stream first and discarded
— so a program without a diverging arm is the program the generator drew
before, and a program with one differs from it only there. -/
def side {α : Type} (m : G α) : G α := do
  modify fun (g, h) => (h, g)
  let r ← m
  modify fun (g, h) => (h, g)
  return r

/-- (helper) True with probability `num / den`. -/
def chance (num den : Nat) : G Bool := do
  let k ← nat 1 den
  return k ≤ num

/-- (helper) One element of a non-empty list, uniformly; `default` for an
empty one. -/
def pick {α : Type} (default : α) (xs : List α) : G α := do
  match xs with
  | [] => return default
  | _ =>
      let i ← nat 0 (xs.length - 1)
      return xs[i]?.getD default

/-- (helper) Walk a weighted list with a number drawn in `[1, total]`. -/
def pickWeighted {α : Type} (default : α) (r : Nat) : List (Nat × α) → α
  | [] => default
  | (w, a) :: rest => if r ≤ w then a else pickWeighted default (r - w) rest

/-- (helper) One element of a weighted list; a weight of zero is never
chosen, so an unavailable form is listed with weight zero rather than
omitted. -/
def weighted {α : Type} (default : α) (choices : List (Nat × α)) : G α := do
  let total := choices.foldl (fun acc (w, _) => acc + w) 0
  if total = 0 then return default
  let r ← nat 1 total
  return pickWeighted default r choices

/-- (helper) The de Bruijn indices of the binders satisfying `p`. -/
def indicesWhere (Γ : Scope) (p : Binder → Bool) : List Nat :=
  let rec go : List Binder → Nat → List Nat
    | [], _ => []
    | b :: bs, i => if p b then i :: go bs (i + 1) else go bs (i + 1)
  go Γ 0

/-- (helper) Whether a type is a struct type. -/
def isStruct : Ty → Bool
  | .struct _ => true
  | _ => false

/-- (helper) Whether a type is an enum type. -/
def isEnum : Ty → Bool
  | .enum _ => true
  | _ => false

/-- (helper) Whether a type is an array type. -/
def isArray : Ty → Bool
  | .array _ _ => true
  | _ => false

/-- (helper) Whether a type is one §6.11 drops by walking into it: a struct,
whose droppable fields go in declaration order (`3.9:13`), an enum, whose
**active** variant's payload goes and nothing else (`6.3:20`), or an array,
whose elements go in ascending index order (`3.9:15`). This is the set a drawn
`@drop` prefers, because a drop of a scalar observes nothing. -/
def isAggregate (T : Ty) : Bool := isStruct T || isEnum T || isArray T

/-- (helper) An integer type: every width and signedness of §2's `int(w, s)`,
with `i64` a little more likely so most cases stay at the width a reader
expects. -/
def intTy : G Ty := do
  let w ← weighted IntWidth.w64 [(1, IntWidth.w8), (1, .w16), (1, .w32), (2, .w64)]
  let sg ← weighted Sign.signed [(2, Sign.signed), (1, .unsigned)]
  return .int w sg

/-- (helper) An integer literal of the wanted type: small, with an occasional
`min_T`/`max_T` so that `+ - * /` can trap (§6.4). A small value is in range
at every width, so the draw needs no per-width case. -/
def intLiteral (w : IntWidth) (sg : Sign) : G Expr := do
  let k ← nat 0 39
  let n : Int :=
    if k = 0 then intMax w sg else if k = 1 then intMin w sg else Int.ofNat (k % 10)
  return intLit w sg n

/-- (helper) A float width: `f64` more often than `f32`, the way `3.12:8`
defaults an unsuffixed literal. -/
def floatWidth : G FloatWidth := weighted FloatWidth.w64 [(1, FloatWidth.w32), (2, FloatWidth.w64)]

/-- (helper) A float type at a drawn width. -/
def floatTy : G Ty := do
  return .float (← floatWidth)

/-- (helper) A float literal (§2's decimal form): a small decimal, sometimes
with a negative exponent so the value is inexact at both widths, and
sometimes `0` so that a division can reach an infinity or a NaN (`3.12:22`).
Every draw is finite and far inside both ranges, which is what `3.12:10`
requires of a source literal — and, because it is finite, **never a NaN**,
which is what makes a literal operand safe under `@total_cmp` (the
`@total_cmp` arm of `expr`). -/
def floatLiteral (w : FloatWidth) : G Expr := do
  let k ← nat 0 19
  let l : FloatLit :=
    if k = 0 then { sig := 0, negExp := false, e := 0 }
    else if k ≤ 6 then { sig := k, negExp := false, e := 0 }
    else if k ≤ 13 then { sig := k, negExp := true, e := 1 }
    else { sig := k, negExp := true, e := 2 }
  return floatLit w l

/-- (helper) An array length (`7.1:14`'s compile-time constant): small, so a
program stays readable, and `0` among them — the zero-sized `[T; 0]`, whose
class is `Affine` rather than the element's when the element is not `Copy`
(§3's table), and at which every dynamic index is out of bounds. -/
def arrayLen : G Nat := weighted 2 [(1, 0), (2, 1), (3, 2), (2, 3)]

/-- (helper) An array type over a drawn element type, one level in four
**nested** — `[[T; m]; n]`, which is what gives a place two index steps
(`a[c][c']`, `a[i][j]`). A `unit` element is not wrapped: `[(); n]` is a
well-formed type, but it holds nothing a program could observe. -/
def arrayOf (T : Ty) : G Ty := do
  if T == .unit then return T
  let n ← arrayLen
  if ← chance 1 4 then return .array (.array T (← arrayLen)) n
  return .array T n

/-! ## Struct and enum declarations

A generated program declares its own structs and enums (`Syntax.lean`), and the
declarations are drawn so that `WfDecls` holds by construction: a field or a
payload component names only a declaration drawn **before** it, which is
`3.0:5`'s acyclicity (E0483) restricted to the draw order; a struct's recorded
class is §3's join lifted by the attribute and an enum's is `6.3:19`'s payload
join over every variant, with no attribute to lift; a destructor is dropped
from the draw when a field carries a linear value (`3.9:44`) or the
declaration is declared `linear` (a choice rather than a rule: `3.9:34` would
refuse every destructure of it), and a `@copy` draw is downgraded to no
attribute when the join is not already `Copy` or the declaration has a
destructor (`3.8:18`, `3.9:31`). So whatever the checker
rejects, it rejects for an ownership reason, never for an ill-formed
declaration.

The draw order is structs, then enums, then a few more structs — `genCase`. A
first-round struct names only earlier structs, an enum names any of those and
any earlier enum, and a last-round struct may also name an enum, which is what
puts an enum in a **field** and so makes a `match` scrutinee a projection. No
cycle is expressible, because every one of those relations points strictly
backwards in one global order. -/

/-- (helper) §3's field join, for a field list read against `D`. -/
def fieldJoin (D : Decls) (fields : List Ty) : Mult :=
  fields.foldl (fun m T => m.join (Ty.mult D T)) .copy

/-- (helper) One field type of declaration `s`, drawn against the `nEnums`
enums already in the environment: a scalar, an earlier struct declaration, or
one of those enums — never `s` itself, a later struct, or a later enum, so the
environment stays acyclic (`3.0:5`). The enum share is `0` for the structs
drawn before any enum exists, which is every first-round declaration. -/
def fieldTyBase (s nEnums : Nat) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(4, ← intTy), (1, .bool), (1, .unit)]
  if s = 0 && nEnums = 0 then
    scalar
  else
    let k ← nat 1 10
    if k ≤ 4 && s ≠ 0 then
      let j ← nat 0 (s - 1)
      return .struct j
    else if k ≤ 8 && nEnums ≠ 0 then
      let j ← nat 0 (nEnums - 1)
      return .enum j
    else
      scalar

/-- (helper) One field type of declaration `s`: `fieldTyBase`'s draw, wrapped
in an array one time in five (`arrayOf`), which is what puts an array in a
**field** — the `h.arr[c]` and `h.arr[i]` places, and `3.8:68`'s refusal of an
element move out of an array reached through a projection. An array names the
same earlier declarations its element does, so `3.0:5`'s order is kept. The
wrap is drawn after the base, so the base's own weights keep their meaning. -/
def fieldTy (s nEnums : Nat) : G Ty := do
  let T ← fieldTyBase s nEnums
  if ← chance 1 5 then arrayOf T else return T

/-- (helper) One struct declaration, well-formed by construction. -/
def genDecl (D : Decls) (s nEnums : Nat) : G StructDecl := do
  let k ← nat 1 3
  let fields ← (List.range k).mapM (fun _ => fieldTy s nEnums)
  let drawnDtor ← chance 1 2
  let drawn ← weighted Attr.none [(4, Attr.none), (1, Attr.copy), (2, Attr.linear)]
  let base := fieldJoin D fields
  -- `3.9:44` (E0462): a destructor is only legal when no field carries a
  -- linear value; `3.8:18`/`3.9:31`: `@copy` needs a `Copy` join and no
  -- destructor. A draw the rules forbid falls back rather than being retried,
  -- so generation stays a pure function of the seed. A declaration drawn
  -- `linear` gets no destructor although one would be well-formed: `3.9:34`
  -- refuses every destructure of a destructor-bearing struct, so a destructor
  -- would make each destructure of it the same E0456 (RUE-2339). Two shapes
  -- are lost with it: a declared-`linear` destructor running in a random
  -- program, and E0456 at the destructured place `d` itself. The seed corpus
  -- keeps the second (`destructure_under_dtor`).
  let dtor := drawnDtor && base != .linear && drawn != .linear
  let attr := match drawn with
    | .copy => if base = .copy && !dtor then Attr.copy else Attr.none
    | a => a
  return { attr := attr, fields := fields, dtor := dtor, cls := attr.lift base }

/-- (helper) `n` more struct declarations, built left to right so each one sees
the ones before it, and drawn against the `nEnums` enums already in the
environment. -/
def genEnv (nEnums : Nat) : Nat → Decls → G Decls
  | 0, acc => return acc
  | n + 1, acc => do
      let sd ← genDecl acc acc.structs.length nEnums
      genEnv nEnums n { acc with structs := acc.structs ++ [sd] }

/-- (helper) One payload component type of enum declaration `e`: a scalar, any
of the `nStructs` struct declarations — none of which names an enum, since they
are drawn first — or an **earlier** enum. Struct components are the common draw
because they are what carries a destructor and a declared attribute, so the
payload classes span `Copy`, `Affine` and `Linear` and `6.3:19`'s join has
something to say. -/
def payloadTy (nStructs e : Nat) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(3, ← intTy), (1, .bool), (1, .unit)]
  let k ← nat 1 10
  if k ≤ 6 && nStructs ≠ 0 then
    let j ← nat 0 (nStructs - 1)
    return .struct j
  else if k ≤ 8 && e ≠ 0 then
    let j ← nat 0 (e - 1)
    return .enum j
  else
    scalar

/-- (helper) One enum declaration, well-formed by construction: two or three
variants in declaration order — the order a tag indexes and (Match) §5.5's arms
are presented in — each carrying `0`–`2` payload components, and the recorded
class exactly `6.3:19`'s join over every component of every variant, which is
what `checkEnumDecl` pins. An arity-`0` variant is `6.3:14`'s
discriminant-only case, and a declaration all of whose variants are arity `0`
is the duplicable tag enum of `3.8:2`. There is no attribute to draw and no
destructor: §3 gives an enum neither (E0417). -/
def genEnumDecl (D : Decls) (e : Nat) : G EnumDecl := do
  let nv ← weighted 2 [(5, 2), (2, 3)]
  let variants ← (List.range nv).mapM (fun _ => do
    let a ← weighted 1 [(3, 0), (5, 1), (2, 2)]
    (List.range a).mapM (fun _ => payloadTy D.structs.length e))
  let ed : EnumDecl := { variants := variants, cls := .copy }
  return { ed with cls := ed.payloadJoin D }

/-- (helper) An enum environment of `n` declarations, built left to right so
each one sees every struct and the enums before it — which is what keeps
`3.0:5`'s cycle out of the draw. -/
def genEnums : Nat → Decls → G Decls
  | 0, acc => return acc
  | n + 1, acc => do
      let ed ← genEnumDecl acc acc.enums.length
      genEnums n { acc with enums := acc.enums ++ [ed] }

/-- (helper) A canonical value of any type, for the one place a type-directed
draw can run out of depth and still owe a well-typed expression: a literal at a
scalar (§5.8's (Lit)), one initializer per **declared** field at a struct
(`3.6:15`, (Struct-Intro) §5.8), the first variant applied to its own
payload at an enum (`6.3:16`, (Enum-Intro) §5.5), and `n` least elements at
`[T; n]` ((Array-Intro) §5.8). The fuel is `declFuel`: three rounds per
declaration, because `3.0:5`'s acyclicity bounds the declaration chain — the
bound `checkNoCycle` peels with — and a field or a binder wraps its type in at
most two array levels (`arrayOf`), each of which spends a round too.

Before this existed the depth-exhausted fallback was `mkStruct s []`, which is
a struct literal missing its initializers: an **ill-typed** program, rejected
by the checker for a reason that is not ownership and by the compiler for a
field-count error rather than the ownership diagnostic the bridge's refusal
table covers (two of the 200 cases at `--gen 200 --seed 7` were that shape). -/
def leastValue (D : Decls) : Nat → Ty → Expr
  | _, .int w sg => intLit w sg 0
  | _, .float w => floatLit w { sig := 0, negExp := false, e := 0 }
  | _, .bool => boolLit false
  | _, .unit => unitLit
  | 0, .struct s => mkStruct s []
  | fuel + 1, .struct s =>
      match D.structs[s]? with
      | some sd => mkStruct s (sd.fields.map (leastValue D fuel))
      | none => mkStruct s []
  | 0, .enum e => mkEnum e 0 []
  | fuel + 1, .enum e =>
      match D.enums[e]? with
      | some ed => mkEnum e 0 (((ed.variants[0]?).getD []).map (leastValue D fuel))
      | none => mkEnum e 0 []
  | 0, .array T _ => mkArray T []
  | fuel + 1, .array T n => mkArray T (List.replicate n (leastValue D fuel T))

/-- (helper) The fuel `leastValue` is called at: three rounds per declaration
and three more for the binder's own array levels, which `3.0:5`'s acyclicity
and `arrayOf`'s two-level bound make sufficient. -/
def declFuel (D : Decls) : Nat := 3 * (D.structs.length + D.enums.length) + 3

/-- (helper) The field slots of declaration `s` at type `T` that a drawn
**assignment** may target, one step deep (module docstring, "How deep a place
goes"). An assignment destination is not a use, so §4.2 computes no plan for
it and a slot of a struct declared `linear` is offered like any other
(RUE-2339); writing a field in place leaves the declared-`linear` value whole,
and (Assign) §5.2's own premises decide the case. What the draw still refuses
is `3.9:34`'s case, kept from when this test also served the use draw (`pathOk`
is its reading at a whole path): a non-`Copy` field of a declaration that
declares a destructor. -/
def projSlots (D : Decls) (s : Nat) (T : Ty) : List Nat :=
  match D.structs[s]? with
  | none => []
  | some sd =>
      if sd.dtor && T.mult D != .copy then []
      else (List.range sd.fields.length).filter (fun f => sd.fields[f]? == some T)

/-- (helper) The one-step places a drawn **assignment** may target under binder
`i` of type `T`, with the type each holds: a struct's slots `projSlots` offers,
and every constant index `a[c]` of an array — (Assign) §5.2 takes an element
destination with no index premise of its own, and `3.8:72`'s demand that the
array be whole (`assignArrayOk`, E0480) is left to the checker like every other
ownership outcome. A destination under an index is **one** step deep for the
reason the struct slot is (module docstring, RUE-2319). -/
def assignSlots (D : Decls) (i : Nat) : Ty → List (Place × Ty)
  | .struct s =>
      match D.structs[s]? with
      | some sd =>
          (List.range sd.fields.length).filterMap (fun f =>
            match sd.fields[f]? with
            | some Tf => if (projSlots D s Tf).contains f then some (.proj (.var i) f, Tf) else none
            | none => none)
      | none => []
  | .array E n => (List.range n).map (fun c => (.idx (.var i) c, E))
  | .int _ _ | .float _ | .bool | .unit | .enum _ => []

/-- (helper) A place from a root binder of type `T₀` and a path of steps, read
from the root outward — the inverse of `Place.path` (`Syntax.lean`). A step
taken at an array is a constant index (`Place.idx`) and any other a field slot
(`Place.proj`). `Place.path` is blind to the difference, and so is every rule
that reads it (`rootIdxOnly`'s docstring says why), so this only makes the
place say what the printed program says. -/
def placeOfPath (D : Decls) (T₀ : Ty) (i : Nat) (π : List Nat) : Place :=
  go (.var i) T₀ π
where
  go (p : Place) (T : Ty) : List Nat → Place
    | [] => p
    | f :: π =>
        match T with
        | .array E _ => go (.idx p f) E π
        | _ => go (.proj p f) ((T.fieldAt D f).getD .unit) π

/-- (helper) The steps a type has, as single steps: a struct's field slots, and
an array's constant indices `0 … n-1` — §5's `Path[c]`, one production with
`Path.f` (`Ty.fieldAt`). -/
def fieldSlots (D : Decls) : Ty → List Nat
  | .struct s =>
      (match D.structs[s]? with
       | some sd => List.range sd.fields.length
       | none => [])
  | .array _ n => List.range n
  -- An **enum** has no step, and that is the fragment's shape
  -- rather than a gap: §5.6 tracks no path into a payload, `Ty.fieldAt` is
  -- `none` at an enum type, and the only way to a payload component is a
  -- `match` arm's binding (`Syntax.lean`, `6.3:17`).
  | .int _ _ | .float _ | .bool | .unit | .enum _ => []

/-- (helper) Every path of **one or two** steps — field slots and constant
indices alike — under a binder's declared type. Depth 2 is where the path machinery actually recurses —
`OwnSt.get`/`setAt`'s padding, `readAt`/`writeAt`, and §6.11's nested `⊘`-skip
— and one seed case (`deep_path`) is not coverage of it. -/
def paths2 (D : Decls) (T₀ : Ty) : List (List Nat) :=
  (fieldSlots D T₀).flatMap fun f =>
    [f] :: (match T₀.fieldAt D f with
            | some T' => (fieldSlots D T').map (fun g => [f, g])
            | none => [])

/-- (helper) Whether the place rules admit a path from a binder of type `T₀` to
a leaf of type `T`: the path types (`atPath`) and, where the leaf is not `Copy`
and so the rule is (Use-Move) or (@Drop) rather than their `Copy` twins, no
proper prefix declares a destructor (`3.9:34`). This is `projSlots`' test read
at a whole path rather than at one step, so it stays right at depth 2.

A path with an **index** step is also held to `3.8:68`'s root rule
(`rootIdxOnly`) at a non-`Copy` leaf: an element is moved or dropped out of the
root binding's array (`a[c]`, `a[c].f`) and of no array reached through a
further step (`h.arr[c]`, `a[c][c']`), which the compiler refuses with E0904.
That premise is (Use-Move)'s and (@Drop)'s, so it is read only where no proper
prefix is a struct declared `linear`: under the declared plan the same path is
a destructure, which `3.8:71` and the compiler admit through any index
(`h.arr[0].x0`). So E0904's element-move refusal is not drawn; a `Copy` read or
`@drop` at `h.arr[c]` or `a[c][c']` is, since (Use-Copy) and (@Drop-Copy)
carry no index premise.

**Which of §4.2's two plans the path selects is not tested here** (RUE-2339).
A path with no proper prefix of declared-`linear` struct type is the `Ordinary`
partial move of `3.8:22`; one with such a prefix is the declared-linear
destructure of `3.8:33`, which (Use-Declared-Linear-Destructure) §5.1 and
(D-Use-Declared-Linear) §6.3 discharge (RUE-2236). Both are drawn from the one
grammar and `declaredPrefix` (`Syntax.lean`) decides between them, which is
what makes the plan's *selection* something the bridge checks rather than
something the draw assumes.

A destructure's own premises are left to chance with every other ownership
choice in this module: a residue that carries a linear value is §5.1's
`¬ linear-residue(S, π_s)` and the E0474 the compiler reports (`3.8:60`), and a
destructor above the leaf is `3.9:34` and E0456 — which the declared plan
demands even at a `Copy` leaf, where the ordinary rules do not. Both are
`reject` verdicts of the kind the bridge's refusal table covers. RUE-2335's
shape, a `@drop` of a declared-`linear` place after a destructure under it, is
not drawn around either: the model accepts it, and the compiler has too since
RUE-2335 was fixed (rare; module docstring). -/
def pathOk (D : Decls) (T₀ : Ty) (π : List Nat) (T : Ty) : Bool :=
  Ty.atPath D T₀ π == some T &&
    (T.mult D == .copy ||
      (noDtorPrefix D T₀ π && (rootIdxOnly D T₀ π || (declaredPrefix D T₀ π).isSome)))

/-- (helper) Every place of the wanted type one **or two** field steps under a
binder in scope: the projections a use may name. -/
def projPlaces (D : Decls) (Γ : Scope) (T : Ty) : List Place :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        ((paths2 D b.ty).filter (fun π => pathOk D b.ty π T)).map (placeOfPath D b.ty i)
    | none => [])).flatten

/-- (helper) Draw a place, biased toward the **deeper** one: where the list
offers a path of two field steps it is taken half the time. Depth 2 is in the
tail without it, because a two-step path needs a nesting declaration, a binder
of the outer type in scope, and — for a move or a `@drop` of a non-`Copy` leaf
— both prefixes free of a destructor. A two-step place counts an index step as
a step (`fieldSlots`). Measured on the current draws, `--gen 200 --seed 7`
reaches 44 depth-2 places across 30 programs with the bias and 37 across 23
without it, `--gen 300 --seed 23` 95 across 50 with and 60 across 39 without,
`--gen 1000 --seed 23` 259 across 145 with and 213 across 139 without, and
`--gen 3000 --seed 101` 687 across 421 with and 584 across 388 without. The
two runs diverge at the first pick the bias changes, so each pair compares two
different sets of programs, and the gain is of the size that noise can make
or undo: on the draws before this module's restoring statements (RUE-2330's
review) the largest run went the other way, 597 against 636. The weight is
left as it is. This is one of the module's weights; it lives
here rather than at the draw sites that pick a place. -/
def pickPlace (default : Place) (ps : List Place) : G Place := do
  let deep := ps.filter (fun p => 2 ≤ p.path.length)
  if !deep.isEmpty && (← chance 1 2) then pick default deep else pick default ps

/-- (helper) Every place one **or two** field steps under a binder in scope,
whatever its type: the projections a `@drop` may name. -/
def dropPlaces (D : Decls) (Γ : Scope) : List Place :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        (paths2 D b.ty).filterMap (fun π =>
          match Ty.atPath D b.ty π with
          | some T => if pathOk D b.ty π T then some (placeOfPath D b.ty i π) else none
          | none => none)
    | none => [])).flatten

/-- (helper) Whether a path takes a step at an array node — a constant index —
read off the type it starts at, as `rootIdxOnly` reads it. -/
def idxStep (D : Decls) : Ty → List Nat → Bool
  | _, [] => false
  | .array _ _, _ :: _ => true
  | T, f :: π =>
      match T.fieldAt D f with
      | some T' => idxStep D T' π
      | none => false

/-- (helper) A place **below a dynamic index** in `Expr.indexRead`'s shape
(RUE-2342): the constant place `p` the first dynamic step indexes, the constant
path `πs` after each dynamic step, the length of the array each dynamic step
indexes (so an index can be drawn in or out of its bounds), and the leaf's type.
-/
structure DynPlace where
  p : Place
  πs : List (List Nat)
  lens : List Nat
  leaf : Ty

/-- (helper) The dynamic tails under an array type, at most `fuel` dynamic
steps: after each step a constant path of zero or one step (a field of the
element, or a constant index of it), and a further dynamic step where that
reaches an array again. So `a[i]`, `a[i].f`, `a[i][c]`, `a[i][j]` and
`a[i].f[j]` are all shapes of it (`Ty.atDyn`'s grammar). -/
def dynTails (D : Decls) : Nat → Ty → List (List (List Nat) × List Nat × Ty)
  | 0, _ => []
  | fuel + 1, .array E n =>
      ([] :: (fieldSlots D E).map (fun f => [f])).flatMap (fun π =>
        match E.atPath D π with
        | some L => ([π], [n], L) :: (dynTails D fuel L).map (fun t => (π :: t.1, n :: t.2.1, t.2.2))
        | none => [])
  | _ + 1, .int _ _ | _ + 1, .float _ | _ + 1, .bool | _ + 1, .unit
  | _ + 1, .struct _ | _ + 1, .enum _ => []

/-- (helper) Every place below a dynamic index that the scope offers, with at
most two dynamic steps: the array indexed first is a binder itself or one of
its one- or two-step constant places (`h.arr`, `a[c]`), so `a[i]`,
`a[i].f`, `h.arr[i].f` and `a[i][j]` are all offered. Nothing is filtered by
ownership or by plan: `fully-owned` at `p`, `3.8:70`'s moved-out element, and
§4.2's `DeclaredLinearDynamic` (a declared-`linear` struct above or below the
index, E0904) are the checker's to refuse. -/
def dynPlaces (D : Decls) (Γ : Scope) : List DynPlace :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        ([] :: paths2 D b.ty).flatMap (fun π =>
          match Ty.atPath D b.ty π with
          | some Ta =>
              (dynTails D 2 Ta).map (fun t =>
                ({ p := placeOfPath D b.ty i π, πs := t.1, lens := t.2.1, leaf := t.2.2 } : DynPlace))
          | none => [])
    | none => [])).flatten

/-- (helper) Draw a place below a dynamic index, biased toward the shapes
RUE-2342 added: half the time one whose first array is reached through a step
(`h.arr[i]`, `a[c][i]`), that has a second dynamic step (`a[i][j]`), or that
goes below an index (`a[i].f`), where the list offers one. -/
def pickDyn (d₀ : DynPlace) (ds : List DynPlace) : G DynPlace := do
  let deep := ds.filter (fun d =>
    !d.p.path.isEmpty || 2 ≤ d.πs.length || d.πs.any (fun π => !π.isEmpty))
  if !deep.isEmpty && (← chance 1 2) then pick d₀ deep else pick d₀ ds

/-- (helper) One index for a dynamic step into an array of length `n`, and the
integer type it is drawn at — any width and signedness (`4.11:4`). A fair share
is **out of bounds** so that (D-Index-Trap) §6.5 is exercised: one draw in five
is `-1` (at a signed type, half the time) or `n`, seven in ten are a value in
`[0, n)` — which at `n = 0` is none, so the zero-sized array always traps —
and one in ten is `other`'s arbitrary expression of the index type, a binder or
a projection or a literal, whatever the draw it stands for finds. -/
def dynIdx (other : Ty → G Expr) (n : Nat) : G (Ty × Expr) := do
  let T ← intTy
  match T with
  | .int w sg =>
      let k ← nat 1 10
      if k ≤ 2 then
        if sg == .signed && (← chance 1 2) then return (T, intLit w sg (-1))
        return (T, intLit w sg n)
      if k ≤ 9 then
        if n = 0 then return (T, intLit w sg 0)
        return (T, intLit w sg (← nat 0 (n - 1)))
      return (T, ← other T)
  | _ => return (.int .w64 .signed, intLit .w64 .signed 0)

/-- (helper) A place with its root moved `k` binders out, for a place drawn
under `Γ` and used under `k` more binders. -/
def liftPlace (k : Nat) : Place → Place
  | .var i => .var (i + k)
  | .proj p f => .proj (liftPlace k p) f
  | .idx p c => .idx (liftPlace k p) c

/-- (helper) Draw one index per dynamic step (`dynIdx`), each **bound by a
`let`** before the form that uses it, and hand the form the extended scope and
the index uses. The binding is not decoration. The printer writes an index in
place as a typed block, `a[{ let t: T = e; t }]`, and where `e` is a literal the
compiler folds that block to a **constant** index — `8.2:4` makes an
expression that can be fully evaluated at compile time one — and bounds-checks
it at compile time: an out-of-range one is E0902, not the run-time trap the
core's dynamic form reaches (`--gen 200 --seed 7` had seven such cases before
this existed). A `let`-bound index is not folded (`let i: i64 = 2; a[i]` traps
at run time), so the core's dynamic index stays dynamic in the printed
program. That rests on the compiler's current reading of `8.2:4`, whose list is
open about an immutable `let` bound to a literal (RUE-2349).
The price is evaluation order: the indices are evaluated before a write's
right-hand side rather than after it (`5.2:14`), which the seed cases
`array_dyn_write_rhs_first` and `array_dyn_write_trap_negative` cover
instead. Each index is drawn under the ones bound before it. -/
def bindIdx (other : Scope → Ty → G Expr) : Scope → List Nat → G (List Expr × Scope)
  | Γ, [] => return ([], Γ)
  | Γ, n :: ns => do
      let (T, e) ← dynIdx (other Γ) n
      let (rest, Γ') ← bindIdx other ({ ty := T, mu := false } :: Γ) ns
      return (e :: rest, Γ')

/-- (helper) A dynamic-index form at place `d`, its indices bound first
(`bindIdx`): `mk` builds the form from the extended scope, the place lifted
under the new binders, and the index uses, first index outermost. -/
def withIdx (Γ : Scope) (d : DynPlace) (other : Scope → Ty → G Expr)
    (mk : Scope → Place → List Expr → G Expr) : G Expr := do
  let (es, Γ') ← bindIdx other Γ d.lens
  let k := es.length
  let uses := (List.range k).map (fun j => use (.var (k - 1 - j)))
  let body ← mk Γ' (liftPlace k d.p) uses
  return es.foldr (fun e acc => letIn false e acc) body

/-- (helper) A dynamic-index **read** of the wanted type, one draw in three
where the scope offers a place below a dynamic index at that type: §4.2's
`Untrackable(OrdinaryDynamic)` plan, whose only rule is
(Use-Untrackable-Dynamic-Copy) §5.1, so it is drawn at a `Copy` leaf only.
`other` draws an index expression that is not a literal. -/
def dynRead (D : Decls) (Γ : Scope) (T : Ty) (other : Scope → Ty → G Expr) :
    G (Option Expr) := do
  if T.mult D != .copy then return none
  let ds := (dynPlaces D Γ).filter (fun d => d.leaf == T)
  match ds with
  | [] => return none
  | d₀ :: _ =>
      if !(← chance 1 3) then return none
      let d ← pickDyn d₀ ds
      return some (← withIdx Γ d other (fun _ p idx => return indexRead p idx d.πs))

/-- (helper) The dynamic forms at type `unit`, one draw in `den` where the
scope offers a place for one, chosen by weight among those it offers: a write
`p[i]… = e` below a dynamic index into a `mut` binder (weight 2; any leaf type,
because (Assign) §5.2's linear-overwrite premise and `3.8:72`'s whole-array
demand are the checker's), a **read** of an observable `Copy` leaf under
`@dbg` (weight 2), and `@drop(p[i]…)` at a `Copy` leaf, (@Drop-Copy) §5.3 below
a dynamic index (weight 1).

The read is here because `atom`'s read has to match the type the draw
wants, and a wanted integer type is one of eight, so without a read whose type
is the place's own the read is rare: without it 14 of the 200 programs at
`--gen 200 --seed 7` and 55 of the 1,000 at `--gen 1000 --seed 23` read below a
dynamic index, with it 36 and 145. `rhs` draws the written value and `other` a
non-literal index, each under the scope it is given. -/
def dynUnit (D : Decls) (Γ : Scope) (den : Nat) (rhs : Scope → Ty → G Expr)
    (other : Scope → Ty → G Expr) : G (Option Expr) := do
  let ds := dynPlaces D Γ
  let ws := ds.filter (fun d => ((Γ[d.p.root]?).map Binder.mu).getD false)
  let cs := ds.filter (fun d => d.leaf.mult D == .copy)
  let rs := cs.filter (fun d => d.leaf.observable)
  match ds with
  | [] => return none
  | d₀ :: _ =>
      if !(← chance 1 den) then return none
      -- `weighted` answers its default when every weight is zero, so the
      -- default is a fourth form that draws nothing: a place whose root is not
      -- `mut` and whose leaf is not `Copy` offers none of the three.
      let form ← weighted 3
        [(if ws.isEmpty then 0 else 2, 0), (if rs.isEmpty then 0 else 2, 1),
          (if cs.isEmpty then 0 else 1, 2)]
      match form with
      | 0 =>
          let d ← pickDyn d₀ ws
          return some (← withIdx Γ d other (fun Γ' p idx => do
            return indexWrite p idx d.πs (← rhs Γ' d.leaf)))
      | 1 =>
          let d ← pickDyn d₀ rs
          return some (← withIdx Γ d other (fun _ p idx => return dbg (indexRead p idx d.πs)))
      | 2 =>
          let d ← pickDyn d₀ cs
          return some (← withIdx Γ d other (fun _ p idx => return indexDrop p idx d.πs))
      | _ => return none

/-- (helper) Every place one or two steps under a binder in scope whose path
takes a **constant index** step, with the type it holds: `a[c]`, `a[c].f`,
`h.arr[c]`, `a[c][c']`. -/
def idxPlaces (D : Decls) (Γ : Scope) : List (Place × Ty) :=
  ((List.range Γ.length).map (fun i =>
    match Γ[i]? with
    | some b =>
        ((paths2 D b.ty).filter (idxStep D b.ty)).filterMap (fun π =>
          (Ty.atPath D b.ty π).map (fun T => (placeOfPath D b.ty i π, T)))
    | none => [])).flatten

/-- (helper) Whether the scope offers an array to index: a place below a
dynamic index or a constant-index place. Where it does, `expr` weights up an
**array statement** (`arrayStmt`), the way it weights up a `match` where an
enum place is in scope. -/
def arrayInScope (D : Decls) (Γ : Scope) : Bool :=
  !(dynPlaces D Γ).isEmpty || !(idxPlaces D Γ).isEmpty

/-- (helper) A unit-typed statement at an array place in scope, the form `expr`
weights up where one is (`arrayInScope`): one of `dynUnit`'s dynamic forms
(weight 3), or at a constant-index place a `@dbg` read of an observable `Copy`
leaf, a `@drop` (`pathOk`, so `3.8:68`'s root rule holds at a non-`Copy` leaf),
a write `a[c] = e` one step under a `mut` binder (`assignSlots`), or an element
move discarded as a statement, `a[c];` (`pathOk` again) — weight 1 each. The
statement's ownership outcome is the checker's, as every other draw's is: an
element moved twice, a write into an array with a moved-out element (E0480), a
dynamic index into one (`3.8:70`), a linear element discarded (`3.8:64`). -/
def arrayStmt (D : Decls) (Γ : Scope) (rhs : Scope → Ty → G Expr) (other : Scope → Ty → G Expr) :
    G Expr := do
  let ips := idxPlaces D Γ
  let reads := ips.filter (fun pt => pt.2.observable)
  let drops := ips.filter (fun pt =>
    match Γ[pt.1.root]? with
    | some b => pathOk D b.ty pt.1.path pt.2
    | none => false)
  let moves := drops.filter (fun pt => pt.2.mult D != .copy)
  let writes := ((List.range Γ.length).filter (fun i => ((Γ[i]?).map Binder.mu).getD false)).flatMap
    (fun i => match (Γ[i]?).map Binder.ty with
      | some (.array E n) => assignSlots D i (.array E n)
      | _ => [])
  let dflt : Place × Ty := (.var 0, .unit)
  let form ← weighted 5
    [(if (dynPlaces D Γ).isEmpty then 0 else 3, 0), (if reads.isEmpty then 0 else 1, 1),
      (if drops.isEmpty then 0 else 1, 2), (if writes.isEmpty then 0 else 1, 3),
      (if moves.isEmpty then 0 else 1, 4)]
  match form with
  | 0 => return ((← dynUnit D Γ 1 rhs other).getD unitLit)
  | 1 => return dbg (use (← pick dflt reads).1)
  | 2 => return drop (← pick dflt drops).1
  | 3 =>
      let (pl, T) ← pick dflt writes
      return assign pl (← rhs Γ T)
  | 4 => return seq (use (← pick dflt moves).1) unitLit
  | _ => return unitLit

/-- (helper) The binders an arm's body is drawn under: (Match) §5.5's payload
locals on top of the enclosing scope, in `armCtx`'s order — the tuple
**reversed**, so component `ai` has de Bruijn index `0` — and unmarked, because
§2 gives a pattern binding no `μ` (the compiler's parser rejects `mut` there,
which is what makes `armCtx`'s `mu := false` faithful). -/
def armScope (Ts : List Ty) (Γ : Scope) : Scope :=
  (Ts.map (fun T => ({ ty := T, mu := false } : Binder))).reverse ++ Γ

/-- (helper) Which enum a drawn `match` scrutinizes: one a binder or a
projection in scope already holds, where there is one, so the scrutinee is a
**place** and typing it is (Use-Move)/(Use-Copy) §5.1 at that place — a move
for a non-`Copy` enum, because a scrutinee is a value context (`3.8:7`,
`3.8:76`; `6.3:17` for the payload the arm binds out of it), and what makes a
second `match` on it the E0205 the compiler reports. Otherwise any declared
enum, which the draw then builds as a temporary. -/
def pickEnumIdx (D : Decls) (Γ : Scope) : G Nat := do
  let inScope := (List.range D.enums.length).filter (fun e =>
    !(indicesWhere Γ (fun b => b.ty == .enum e)).isEmpty ||
      !(projPlaces D Γ (.enum e)).isEmpty)
  if !inScope.isEmpty && (← chance 3 4) then pick 0 inScope
  else pick 0 (List.range D.enums.length)

/-- (helper) Whether the scope already holds a place of enum type — a binder,
or a field of a binder. Where it does, a drawn `match` is weighted up, because
a `match` on a **place** is where the interesting ownership lives: the move
that consumes the binding (`3.8:7`, `3.8:76`, `6.3:17`), the partial move at a
field (`3.8:22`), and the E0205 a second `match` on the same place is. A `match` on
a temporary exercises (D-Match) §6.6 and the arm teardown but leaves no state
behind for the join to read. -/
def enumPlaceInScope (D : Decls) (Γ : Scope) : Bool :=
  (List.range D.enums.length).any (fun e =>
    !(indicesWhere Γ (fun b => b.ty == .enum e)).isEmpty ||
      !(projPlaces D Γ (.enum e)).isEmpty)

/-- (helper) The type of a fresh `let` binder before `binderTy`'s array wrap:
mostly aggregates, when the program declares any — structs a little more often
than enums, since a struct is also what an enum's payload is usually made of. -/
def binderTyBase (D : Decls) : G Ty := do
  let scalar : G Ty := do weighted (← intTy) [(2, ← intTy), (1, ← floatTy), (1, .bool)]
  let structTy : G Ty := do
    if D.structs.isEmpty then scalar
    else
      -- A declaration that holds an enum in a field is preferred, because a
      -- binder of that type is what makes a drawn `match` scrutinize a
      -- **projection** — `3.8:22`'s partial move at a field, whose residue
      -- the sibling fields still drop at scope exit (§6.11).
      let holders := (List.range D.structs.length).filter (fun s =>
        ((D.structs[s]?).map (fun sd => sd.fields.any isEnum)).getD false)
      if !holders.isEmpty && (← chance 1 2) then return .struct (← pick 0 holders)
      return .struct (← nat 0 (D.structs.length - 1))
  let enumTy : G Ty := do
    if D.enums.isEmpty then structTy else return .enum (← nat 0 (D.enums.length - 1))
  if D.structs.isEmpty && D.enums.isEmpty then
    scalar
  else
    let k ← nat 1 10
    if k ≤ 4 then structTy
    else if k ≤ 7 then enumTy
    else scalar

/-- (helper) The type of a fresh `let` binder: `binderTyBase`'s draw, wrapped in
an array one time in five (`arrayOf`), so an array binder holds scalars,
structs or enums in the proportions any binder does. The wrap is drawn after
the base, so the base's weights keep their meaning. -/
def binderTy (D : Decls) : G Ty := do
  let T ← binderTyBase D
  if ← chance 1 5 then arrayOf T else return T

mutual
/-- (helper) The smallest expression of a type: a literal, a use of a binder
of that type, a struct, enum or array literal with a leaf per component, or —
at a `Copy` type, one draw in three where the scope offers one — a read below a
dynamic index (`dynRead`), whose index expressions are atoms one level down. -/
def atom (D : Decls) (Γ : Scope) : Ty → Nat → G Expr
  | .int w sg, depth => do
      if let d + 1 := depth then
        if let some e ← dynRead D Γ (.int w sg) (fun Γ' T => atom D Γ' T d) then return e
      let projs := projPlaces D Γ (.int w sg)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .int w sg)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
      intLiteral w sg
  | .float w, depth => do
      if let d + 1 := depth then
        if let some e ← dynRead D Γ (.float w) (fun Γ' T => atom D Γ' T d) then return e
      let uses := indicesWhere Γ (fun b => b.ty == .float w)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
      floatLiteral w
  | .bool, depth => do
      if let d + 1 := depth then
        if let some e ← dynRead D Γ .bool (fun Γ' T => atom D Γ' T d) then return e
      let uses := indicesWhere Γ (fun b => b.ty == .bool)
      if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
      return boolLit (← bool)
  | .unit, _ => return unitLit
  | .struct s, depth => do
      if let d + 1 := depth then
        if let some e ← dynRead D Γ (.struct s) (fun Γ' T => atom D Γ' T d) then return e
      let projs := projPlaces D Γ (.struct s)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .struct s)
      if !uses.isEmpty && (← chance 2 3) then return use (.var (← pick 0 uses))
      match D.structs[s]?, depth with
      | some sd, d + 1 => return mkStruct s (← sd.fields.mapM (fun T => atom D Γ T d))
      | _, _ => return leastValue D (declFuel D) (.struct s)
  | .enum e, depth => do
      -- (Enum-Intro) §5.5, or a use of an enum already in scope. The tag is
      -- drawn uniformly over the declared variants, so a discriminant-only one
      -- (`6.3:14`) is as likely as a payload-carrying one at the same
      -- declaration.
      if let d + 1 := depth then
        if let some e' ← dynRead D Γ (.enum e) (fun Γ' T => atom D Γ' T d) then return e'
      let projs := projPlaces D Γ (.enum e)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .enum e)
      if !uses.isEmpty && (← chance 2 3) then return use (.var (← pick 0 uses))
      match D.enums[e]?, depth with
      | some ed, d + 1 =>
          let k ← nat 0 (ed.variants.length - 1)
          return mkEnum e k (← ((ed.variants[k]?).getD []).mapM (fun T => atom D Γ T d))
      | _, _ => return leastValue D (declFuel D) (.enum e)
  | .array T n, depth => do
      -- A use of an array place in scope — a projection (`h.arr`, `a[c]`) or a
      -- binder, with the struct arm's weights — or (Array-Intro) §5.8's
      -- literal, one atom per element, or at a `Copy` element one time in three
      -- the repeat form `[e; n]` (`7.1:36`, `7.1:38`), which the checker
      -- refuses at any other element class (E0905) and so is not drawn there.
      let projs := projPlaces D Γ (.array T n)
      if !projs.isEmpty && (← chance 1 2) then return use (← pickPlace (.var 0) projs)
      let uses := indicesWhere Γ (fun b => b.ty == .array T n)
      if !uses.isEmpty && (← chance 2 3) then return use (.var (← pick 0 uses))
      match depth with
      | d + 1 =>
          if T.mult D == .copy && (← chance 1 3) then return repeatArray T (← atom D Γ T d) n
          return mkArray T (← (List.replicate n T).mapM (fun T' => atom D Γ T' d))
      | 0 => return leastValue D (declFuel D) (.array T n)

/-- (helper) A leaf of the wanted type, one level at most: an atom, a `@drop`
of a place, or an assignment of an atom to one. -/
def leaf (D : Decls) (Γ : Scope) : Ty → Nat → G Expr
  | .unit, depth => do
      let aggregates := indicesWhere Γ (fun b => isAggregate b.ty)
      let muts := indicesWhere Γ (fun b => b.mu)
      let drops := dropPlaces D Γ
      let form ← weighted 0
        [(1, 0), (if Γ.isEmpty then 0 else 6, 1), (if muts.isEmpty then 0 else 5, 2)]
      match form with
      | 1 =>
          -- A dynamic-index form first, one draw in four where the scope has
          -- a place for one (`dynUnit`); the rest keep their weights.
          if let some e ← dynUnit D Γ 4 (fun Γ' T => atom D Γ' T depth) (fun Γ' T => atom D Γ' T depth) then
            return e
          if !drops.isEmpty && (← chance 1 2) then return drop (← pickPlace (.var 0) drops)
          if !aggregates.isEmpty && (← chance 3 4) then return drop (.var (← pick 0 aggregates))
          return drop (.var (← nat 0 (Γ.length - 1)))
      | 2 =>
          let i ← pick 0 muts
          let b := Γ[i]?.getD ⟨.int .w64 .signed, true⟩
          let slots := assignSlots D i b.ty
          if !slots.isEmpty && (← chance 1 2) then
            let (pl, Tf) ← pick (.var i, b.ty) slots
            return assign pl (← atom D Γ Tf depth)
          return assign (.var i) (← atom D Γ b.ty depth)
      | _ => return unitLit
  | T, depth => atom D Γ T depth
end

/-- (helper) A **break arm**: the arm of an `if` or a `match` inside a loop
body, or the tail of a once-through loop's body, that leaves the loop. Half the
time, where the scope holds a binder of a non-`Copy` aggregate type, it is
`{ @drop(x); break }` for such a binder — a move inside a loop that meets §5.7's
exit rather than its back edge, which is RUE-1615's shape when every path
breaks and RUE-1614's exit join when another exit keeps `x`; otherwise a bare
`break` or one unit-typed leaf and then `break`. The `break` is the arm's
**last** form, so no syntax follows it in its block (RUE-2376), and the arm is
`never`-typed, which (Sub-Never) §5.7 coerces to the other arms' type
(`firstArmTy` and `CTy.meet` skip it, `Checker.lean`). -/
def breakArm (D : Decls) (Γ : Scope) : G Expr := do
  let owned := indicesWhere Γ (fun b => isAggregate b.ty && b.ty.mult D != .copy)
  if !owned.isEmpty && (← chance 1 2) then return seq (drop (.var (← pick 0 owned))) brk
  if ← chance 1 2 then return brk
  return seq (← leaf D Γ .unit 2) brk

/-- (helper) A **return or panic arm** (RUE-2383): the arm of an `if` or a
`match`, or the arm of a loop's exit statement, that leaves the function
rather than the loop — `return v` two times in three, with `v` an atom at the
enclosing function's return type `R` ((Return-Value) §5.7, (D-Return) §6.9),
and `@panic("gen")` otherwise ((Panic) §5.8, (D-Panic) §6.12). As a break arm
does, it is the bare form half the time and otherwise a block whose **last**
form it is: `{ @drop(x); … }` at a non-`Copy` aggregate binder, where the
scope holds one, or `{ <leaf>; … }` — so no syntax follows it (RUE-2376). It
is `never`-typed, as a break arm is, and it is drawn only where a break arm may
stand, never inside an operand (`expr`'s `rt`). Every draw here reads the side
stream (`side`). -/
def divArm (D : Decls) (R : Ty) (Γ : Scope) : G Expr := side do
  let tail ← do
    if ← chance 2 3 then pure (ret (← atom D Γ R 2)) else pure (Expr.panic "gen")
  if ← chance 1 2 then return tail
  let owned := indicesWhere Γ (fun b => isAggregate b.ty && b.ty.mult D != .copy)
  if !owned.isEmpty && (← chance 1 2) then return seq (drop (.var (← pick 0 owned))) tail
  return seq (← leaf D Γ .unit 2) tail

/-- (helper) Turn an arm drawn on the main stream into a return or panic arm
(`divArm`), one time in `den` where `rt` allows one; the choice reads the side
stream, and the arm it replaces has already been drawn, so the main stream is
where it would have been either way (`side`). -/
def maybeDiv (D : Decls) (R : Ty) (rt : Bool) (Γ : Scope) (den : Nat) (a : Expr) : G Expr := do
  if rt && (← side (chance 1 den)) then divArm D R Γ else return a

/-- (helper) Which arm of a `match` inside a loop body is a break arm, one
`match` in three: at most **one**, so a `match` of two or more arms always has
an arm that continues and never diverges as a whole (RUE-2376). Outside a loop
body (`lb` false) there is none, because a `break` there targets no loop. -/
def breakArmIdx (lb : Bool) (n : Nat) : G (Option Nat) := do
  if lb && 2 ≤ n && (← chance 1 3) then return some (← nat 0 (n - 1))
  return none

/-- (helper) The loop the generator draws, around a body drawn under the
counter: `let mut k: i64 = 0; loop { if k >= n { break } else { () }; k = k +
1; body }`. The guard is the body's first statement, so every turn that
reaches it with `k ≥ n` exits, and the body runs at most `n` times whatever it
does — the bound that makes every generated loop terminate (module docstring,
"Loops"). -/
def countedLoop (n : Nat) (body : Expr) : Expr :=
  let k := use (.var 0)
  letIn true (intLit .w64 .signed 0)
    (loop
      (seq (ite (binop .ge k (intLit .w64 .signed n)) brk unitLit)
        (seq (assign (.var 0) (binop .add k (intLit .w64 .signed 1))) body)))

/-- (helper) A **restoring statement** for a counted loop's body, drawn at a
`mut` binder `i` of struct or enum type from outside the loop, whose value it
moves and replaces within the turn (`3.8:79`'s two idioms, §5.7): at an
`Affine` type **reassign-then-move**, `x = <fresh>; @drop(x)`, so `x` is
`MovedOut` at the back edge and the loop-head state, and (Assign)
reinitializes it before the move; at a `Linear` type **move-then-reassign**,
`@drop(x); x = <fresh>`, so the back edge restores the entry state — the
first idiom would overwrite a live linear value on the first turn (`3.8:77`).
The fresh value is `leastValue`'s literal, which names no binder, so the
statement moves nothing but `x`. -/
def restoreStmt (D : Decls) (i : Nat) (T : Ty) : Expr :=
  let fresh := leastValue D (declFuel D) T
  if T.mult D == .linear then seq (drop (.var i)) (assign (.var i) fresh)
  else seq (assign (.var i) fresh) (drop (.var i))

/-- (helper) Draw a loop under `Γ` (module docstring, "Loops"): counted three
times in four (`countedLoop`, a bound `n ≤ 3`) and **once-through** otherwise —
a body whose last form is a break arm, so it runs once and no turn reaches the
back edge. `body` draws the body and `cond` a condition, each under the scope
the body sees: a counted loop's has the counter on top, **unmarked**, so that
nothing the body draws writes it. Half the bodies open with an **exit
statement**, `if c { … break } else { () }`: an exit the run may or may not
take, beside the loop's own.

Two draws aim at the shapes a random body seldom reaches **accepted**. Half the
counted bodies where the scope offers one open with a restoring statement
(`restoreStmt`), and half of those are that statement alone: a move across the
back edge the head state admits. And half
the once-through loops where the scope holds a linear struct or enum binder
**consume it on every exit**: the exit statement's arm and the tail are both
`{ @drop(x); break }`, so §5.7's exit join agrees on `x` — the accepted side of
RUE-1614's rule, where one exit keeping `x` would be E0443; half of those have
an empty body before the exits. -/
def drawLoop (D : Decls) (R : Ty) (rt : Bool) (Γ : Scope) (body cond : Scope → G Expr) :
    G Expr := do
  let n ← nat 0 3
  let once ← chance 1 4
  let Γl : Scope := if once then Γ else { ty := .int .w64 .signed, mu := false } :: Γ
  let b ← body Γl
  let structOrEnum (T : Ty) : Bool := isStruct T || isEnum T
  let b ← if once then pure b else do
      let rs := indicesWhere Γl (fun bd => bd.mu && structOrEnum bd.ty && bd.ty.mult D != .copy)
      if !rs.isEmpty && (← chance 1 2) then
        let i ← pick 0 rs
        let r := restoreStmt D i (((Γl[i]?).map Binder.ty).getD .unit)
        -- Half of these bodies are the restoring statement alone, so that a
        -- random rest cannot undo it (a second move of `x` is the usual one).
        if ← chance 1 2 then pure r else pure (seq r b)
      else pure b
  let lins := indicesWhere Γl (fun bd => structOrEnum bd.ty && bd.ty.mult D == .linear)
  let consume ← if once && !lins.isEmpty && (← chance 1 2) then some <$> pick 0 lins
    else pure none
  -- Likewise half of the consuming bodies are empty before their exits.
  let b ← if consume.isSome && (← chance 1 2) then pure unitLit else pure b
  let arm : G Expr := match consume with
    | some i => pure (seq (drop (.var i)) brk)
    | none => breakArm D Γl
  let b ← if ← chance 1 2 then do
      let c ← cond Γl
      -- The exit statement's arm, unless it consumes `x` on every exit, may
      -- leave the function instead (`maybeDiv`, RUE-2383): a `return` or
      -- `@panic` inside a loop. The tail below never does, because a loop
      -- whose every exit left the function would diverge as a whole and the
      -- rest of the block after it would be dead code (RUE-2376).
      let a ← arm
      let a ← if consume.isSome then pure a else maybeDiv D R rt Γl 3 a
      pure (seq (ite c a unitLit) b)
    else pure b
  if once then return loop (seq b (← arm))
  return countedLoop n b

/-- (helper) A `match`'s arms, one per variant of `ed` in declaration order,
each under its payload locals (`armScope`): arm `bj` is a break arm
(`breakArmIdx`) and every other arm is `other`'s draw, all on the main stream.
Then, where `rt` allows one, the side stream (`side`) turns one arm into a
return or panic arm (`divArm`, RUE-2383): the break arm one time in three,
and in a `match` without one a random arm one time in six — so at most one
arm diverges and the `match` never does as a whole (RUE-2376). -/
def divArms (D : Decls) (R : Ty) (rt : Bool) (ed : EnumDecl) (Γ : Scope) (bj : Option Nat)
    (other : List Ty → G Expr) : G (List Expr) := do
  let n := ed.variants.length
  let arms ← (List.range n).mapM (fun j =>
    let Ts := (ed.variants[j]?).getD []
    if bj == some j then breakArm D (armScope Ts Γ) else other Ts)
  let dj ← match bj with
    | some j => do if rt && (← side (chance 1 3)) then pure (some j) else pure none
    | none => do
        if rt && (← side (chance 1 6)) then pure (some (← side (nat 0 (n - 1))))
        else pure none
  match dj with
  | none => return arms
  | some j =>
      let a ← divArm D R (armScope ((ed.variants[j]?).getD []) Γ)
      return arms.set j a

/-- (helper) Whether the scope holds a binder of a non-`Copy` aggregate type:
something a loop body can move or `@drop`, which is what makes a loop's
back edge and exits say anything about ownership. -/
def ownedInScope (D : Decls) (Γ : Scope) : Bool :=
  Γ.any (fun b => isAggregate b.ty && b.ty.mult D != .copy)

/-- (helper) An expression of the wanted type under `Γ`, at most `fuel`
levels deep. The weights here are the bias the module docstring
describes. -/
def expr (D : Decls) (R : Ty) : Bool → Bool → Scope → Ty → Nat → G Expr
  | _, rt, Γ, T, 0 => do
      -- Out of fuel the draw is a leaf, and that is where most of a
      -- program's binders are in scope: a `let` body is drawn one level down
      -- from the `let`. So where the scope has an array to index, the leaf is
      -- preceded by an array statement half the time (`arrayStmt`, its
      -- operands atoms) — the fuel-0 counterpart of form 5 below.
      if arrayInScope D Γ && (← chance 1 2) then
        let s ← arrayStmt D Γ (fun Γ' T' => atom D Γ' T' 2) (fun Γ' T' => atom D Γ' T' 2)
        return seq s (← leaf D Γ T 2)
      -- Likewise a loop statement one time in four where the scope holds
      -- something a body can move (`ownedInScope`), its body and condition
      -- leaves: this is where a move inside a loop has an outer binder to take.
      if ownedInScope D Γ && (← chance 1 4) then
        let lp ← drawLoop D R rt Γ (fun Γ' => leaf D Γ' .unit 2) (fun Γ' => atom D Γ' .bool 2)
        return seq lp (← leaf D Γ T 2)
      leaf D Γ T 2
  | lb, rt, Γ, T, fuel + 1 => do
      if !Γ.isEmpty && (← chance 1 6) then return (← leaf D Γ T 2)
      let form ← weighted 3
        [(4, 0), (3, 1), (3, 2), (4, 3),
          (if D.enums.isEmpty then 0 else if enumPlaceInScope D Γ then 14 else 3, 4),
          (if arrayInScope D Γ then 6 else 0, 5),
          (if T != .unit then 0 else if ownedInScope D Γ then 4 else 1, 6),
          (if ownedInScope D Γ then 3 else 1, 7)]
      match form with
      | 0 =>
          let T₁ ← binderTy D
          let m ← chance 2 3
          let e₁ ← expr D R lb rt Γ T₁ fuel
          let e₂ ← expr D R lb rt ({ ty := T₁, mu := m } :: Γ) T fuel
          return letIn m e₁ e₂
      | 1 =>
          let muts := indicesWhere Γ (fun b => b.mu)
          let T₁ ← weighted .unit
            [(if muts.isEmpty then 4 else 7, .unit), (2, ← binderTy D), (2, ← intTy)]
          let e₁ ← expr D R lb rt Γ T₁ fuel
          let e₂ ← expr D R lb rt Γ T fuel
          return seq e₁ e₂
      | 2 =>
          let c ← expr D R false false Γ .bool fuel
          -- Inside a loop body, one `if` in three has a **break arm**
          -- (`breakArm`) on a side drawn at random, and the other arm is an
          -- ordinary draw: at most one arm diverges, so the `if` itself never
          -- does and nothing after it is dead code (RUE-2376).
          -- Where `rt` allows one, the break arm is a return or panic arm one
          -- time in three instead, and an `if` without a break arm has one on
          -- a side drawn at random one time in eight (`maybeDiv`, RUE-2383):
          -- still at most one diverging arm.
          if lb && (← chance 1 3) then
            let a ← maybeDiv D R rt Γ 3 (← breakArm D Γ)
            let e ← expr D R lb rt Γ T fuel
            if ← bool then return ite c a e else return ite c e a
          let e₁ ← expr D R lb rt Γ T fuel
          let e₂ ← expr D R lb rt Γ T fuel
          if rt && (← side (chance 1 8)) then
            if ← side bool then return ite c (← divArm D R Γ) e₂
            return ite c e₁ (← divArm D R Γ)
          return ite c e₁ e₂
      | 6 | 7 =>
          -- A `loop` (`drawLoop`). Form 6 is the loop itself, at `unit`; form 7,
          -- at any type, is the loop as a statement before the rest, the way
          -- form 5 places an array statement, because a `unit`-typed draw one
          -- level above a leaf is rare. Both are weighted up where the scope
          -- holds something a body can move (`ownedInScope`).
          let lp ← drawLoop D R rt Γ (fun Γ' => expr D R true rt Γ' .unit fuel)
            (fun Γ' => expr D R false false Γ' .bool fuel)
          if form == 6 then return lp
          return seq lp (← expr D R lb rt Γ T fuel)
      | 5 =>
          -- An array statement, then the rest at the wanted type: weighted up
          -- where the scope has an array to index (`arrayStmt`), because the
          -- type-directed draws reach an index form only where the type they
          -- want is the element's.
          let s ← arrayStmt D Γ (fun Γ' T' => expr D R false false Γ' T' fuel)
            (fun Γ' T' => expr D R false false Γ' T' fuel)
          return seq s (← expr D R lb rt Γ T fuel)
      | 4 =>
          -- (Match) §5.5 in expression position: the scrutinee at the drawn
          -- enum type, then **exactly one arm per variant in declaration
          -- order**, each drawn at the `match`'s own type `T` under that
          -- variant's payload locals (`armScope`). Exhaustiveness is the arm
          -- list's shape, so it is a property of the draw rather than a
          -- premise the draw could miss; what the arm does with its payload
          -- is left to chance, exactly as every other ownership choice is.
          let e ← pickEnumIdx D Γ
          match D.enums[e]? with
          | some ed =>
              -- The scrutinee is a **place** wherever the scope offers one, so
              -- that typing it is the (Use-Move)/(Use-Copy) §5.1 read at that
              -- place rather than the construction of a fresh temporary: a
              -- projection is `3.8:22`'s partial move out of a struct field,
              -- and a binder is what makes a second `match` on it the E0205
              -- use of a moved-out place. A temporary is the remaining draw,
              -- and the one the empty scope always takes.
              let projs := projPlaces D Γ (.enum e)
              let uses := indicesWhere Γ (fun b => b.ty == .enum e)
              if projs.isEmpty && uses.isEmpty && (← chance 1 2) then
                -- The scope offers no place of this type, so half the time the
                -- draw **makes** one: `let v = <the temporary> in match v`,
                -- sometimes with one statement in between. Without this the
                -- place-scrutinee shapes are starved — a `match` is drawn far
                -- more often where no enum place is in scope than where one is
                -- — and with it the arms are typed under a scope that still
                -- holds the consumed binding, which is what a nested `match` on
                -- `v` (E0205) and the join over an entry an arm moved need.
                let m ← chance 1 3
                let init ← expr D R false false Γ (.enum e) fuel
                let Γ' : Scope := { ty := .enum e, mu := m } :: Γ
                let arms ← divArms D R rt ed Γ' (← breakArmIdx lb ed.variants.length)
                  (fun Ts => expr D R lb rt (armScope Ts Γ') T fuel)
                let m' := «match» (use (.var 0)) arms
                let body ← if ← chance 1 3 then
                    pure (seq (← leaf D Γ' .unit 2) m')
                  else pure m'
                return letIn m init body
              let scrut ←
                if !projs.isEmpty && (← chance 3 5) then
                  pure (use (← pickPlace (.var 0) projs))
                else if !uses.isEmpty && (← chance 4 5) then
                  pure (use (.var (← pick 0 uses)))
                else
                  expr D R false false Γ (.enum e) fuel
              let arms ← divArms D R rt ed Γ (← breakArmIdx lb ed.variants.length)
                (fun Ts => expr D R lb rt (armScope Ts Γ) T fuel)
              return «match» scrut arms
          | none => leaf D Γ T 2
      | _ =>
          match T with
          | .enum e =>
              -- The same (Enum-Intro) §5.5 draw `atom` makes, one level up: a
              -- use of an enum in scope, a projection of one out of a struct
              -- field, or a fresh tagged value whose payload arguments are
              -- themselves drawn expressions (§6.2's left-to-right order).
              let uses := indicesWhere Γ (fun b => b.ty == .enum e)
              if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
              let projs := projPlaces D Γ (.enum e)
              if !projs.isEmpty && (← chance 1 3) then return use (← pickPlace (.var 0) projs)
              match D.enums[e]? with
              | some ed =>
                  let k ← nat 0 (ed.variants.length - 1)
                  return mkEnum e k
                    (← ((ed.variants[k]?).getD []).mapM (fun T' => expr D R false false Γ T' fuel))
              | none => return leastValue D (declFuel D) (.enum e)
          | .int w sg =>
              let self := expr D R false false Γ (.int w sg) fuel
              let form ← weighted 0 [(5, 0), (3, 1), (2, 2), (3, 3), (2, 4)]
              match form with
              | 0 =>
                  let op ← pick BinOp.add [BinOp.add, .sub, .mul, .div, .rem]
                  return binop op (← self) (← self)
              | 1 =>
                  let op ← pick BinOp.bitAnd [BinOp.bitAnd, .bitOr, .bitXor, .shl, .shr]
                  return binop op (← self) (← self)
              | 2 =>
                  -- (Neg) §5.8 takes a signed operand only (`4.2:6`), so the
                  -- unsigned draw falls back to the complement.
                  if sg == .signed && (← chance 1 2) then return unop .neg (← self)
                  return unop .bitnot (← self)
              | 3 =>
                  let projs := projPlaces D Γ (.int w sg)
                  if !projs.isEmpty then return use (← pickPlace (.var 0) projs)
                  return binop .add (← self) (← self)
              | _ =>
                  -- `@intCast` from an integer, or `@float_to_int` from a
                  -- float — the one float form that can trap (`3.12:18`) —
                  -- or `@total_cmp`, whose result is `i32` (`3.12:31`).
                  if w == .w32 && sg == .signed && (← chance 1 3) then
                    -- Both operands are **float literals**, not arbitrary
                    -- expressions. `@total_cmp` is the only form that can see
                    -- a NaN's sign, and that sign is `σ_NaN`, a *target*
                    -- parameter (§2, `3.12:44`) — so an expectation that read
                    -- one would be right on one target and wrong on the other.
                    -- `floatLiteral` draws only finite decimals, so no draw
                    -- here can be a NaN, and the restriction is what makes
                    -- that structural rather than a property of the seed.
                    let wf ← floatWidth
                    return binop .totalCmp (← floatLiteral wf) (← floatLiteral wf)
                  if ← chance 1 3 then
                    let Tf ← floatTy
                    return fintrin (.floatToInt w sg) (← expr D R false false Γ Tf fuel)
                  let src ← intTy
                  return intCast w sg (← expr D R false false Γ src fuel)
          | .float w =>
              -- (Float-Arith), (Float-Neg), (Int-To-Float), (Float-Cast) and
              -- (Float-Round) §5.8, with §6.4's trap-free dynamics.
              let self := expr D R false false Γ (.float w) fuel
              let form ← weighted 0 [(5, 0), (2, 1), (2, 2), (2, 3), (2, 4)]
              match form with
              | 0 =>
                  let op ← pick BinOp.add [BinOp.add, .sub, .mul, .div]
                  return binop op (← self) (← self)
              | 1 => return unop .neg (← self)
              | 2 =>
                  let src ← intTy
                  return fintrin (.intToFloat w) (← expr D R false false Γ src fuel)
              | 3 =>
                  -- `3.12:19` converts between the two widths and only
                  -- between them, so the source is the other one.
                  let src : FloatWidth := match w with | .w32 => .w64 | .w64 => .w32
                  return fintrin (.floatCast w) (← expr D R false false Γ (.float src) fuel)
              | _ =>
                  let k ← pick FloatUnIntrin.sqrt
                    [FloatUnIntrin.sqrt, .round .floor, .round .ceil, .round .trunc,
                      .round .round]
                  return fintrin (.roundOp k) (← self)
          | .bool =>
              if ← chance 1 5 then return unop .not (← expr D R false false Γ .bool fuel)
              let op ← pick BinOp.lt [BinOp.lt, .le, .gt, .ge]
              if ← chance 1 3 then
                let Tf ← floatTy
                return binop op (← expr D R false false Γ Tf fuel) (← expr D R false false Γ Tf fuel)
              let Tc ← intTy
              return binop op (← expr D R false false Γ Tc fuel) (← expr D R false false Γ Tc fuel)
          | .unit =>
              let muts := indicesWhere Γ (fun b => b.mu)
              let drops := dropPlaces D Γ
              -- A dynamic-index write or `@drop`, one draw in four where the
              -- scope has a place below a dynamic index (`dynUnit`).
              if let some e ← dynUnit D Γ 4 (fun Γ' T => expr D R false false Γ' T fuel) (fun Γ' T => expr D R false false Γ' T fuel) then
                return e
              -- A `@drop` or an assignment *at a projection* is the shape this
              -- slice is about (§4.2's partial move), so it is drawn first.
              if !drops.isEmpty && (← chance 2 5) then return drop (← pickPlace (.var 0) drops)
              if !muts.isEmpty && (← chance 3 4) then
                let i ← pick 0 muts
                let b := Γ[i]?.getD ⟨.int .w64 .signed, true⟩
                let slots := assignSlots D i b.ty
                if !slots.isEmpty && (← chance 1 2) then
                  let (pl, Tf) ← pick (.var i, b.ty) slots
                  return assign pl (← expr D R false false Γ Tf fuel)
                return assign (.var i) (← expr D R false false Γ b.ty fuel)
              if ← chance 1 3 then
                let To ← weighted (← intTy) [(3, ← intTy), (2, ← floatTy), (1, .bool)]
                return dbg (← expr D R false false Γ To fuel)
              if Γ.isEmpty && !D.structs.isEmpty then
                let T₁ ← binderTy D
                return seq (← expr D R false false Γ T₁ fuel) unitLit
              leaf D Γ .unit 2
          | .struct s =>
              let uses := indicesWhere Γ (fun b => b.ty == .struct s)
              if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
              let projs := projPlaces D Γ (.struct s)
              if !projs.isEmpty && (← chance 1 3) then return use (← pickPlace (.var 0) projs)
              match D.structs[s]? with
              | some sd => return mkStruct s (← sd.fields.mapM (fun T' => expr D R false false Γ T' fuel))
              | none => return mkStruct s []
          | .array E n =>
              -- `atom`'s array draw one level up: a use of an array binder or
              -- of an array place in scope, or a literal — the repeat form at a
              -- `Copy` element one time in three — whose elements are drawn
              -- expressions, left to right (§6.2).
              let uses := indicesWhere Γ (fun b => b.ty == .array E n)
              if !uses.isEmpty && (← chance 1 2) then return use (.var (← pick 0 uses))
              let projs := projPlaces D Γ (.array E n)
              if !projs.isEmpty && (← chance 1 3) then return use (← pickPlace (.var 0) projs)
              if E.mult D == .copy && (← chance 1 3) then return repeatArray E (← expr D R false false Γ E fuel) n
              return mkArray E (← (List.replicate n E).mapM (fun T' => expr D R false false Γ T' fuel))

/-- (helper) Every subexpression, the expression itself first. -/
def subexprs : Expr → List Expr
  | e@(binop _ e₁ e₂) | e@(seq e₁ e₂) | e@(letIn _ e₁ e₂) =>
      e :: subexprs e₁ ++ subexprs e₂
  | e@(.ite c e₁ e₂) => e :: subexprs c ++ subexprs e₁ ++ subexprs e₂
  | e@(assign _ e₁) | e@(ret e₁) | e@(unop _ e₁) | e@(intCast _ _ e₁)
  | e@(fintrin _ e₁) | e@(dbg e₁) => e :: subexprs e₁
  | e@(call _ args) | e@(mkStruct _ args) | e@(mkEnum _ _ args) | e@(mkArray _ args)
  | e@(indexRead _ args _) | e@(indexDrop _ args _) =>
      e :: (args.map subexprs).flatten
  | e@(repeatArray _ e₁ _) | e@(loop e₁) => e :: subexprs e₁
  | e@(indexWrite _ args _ e₁) => e :: subexprs e₁ ++ (args.map subexprs).flatten
  | e@(.«match» scrut arms) => e :: subexprs scrut ++ (arms.map subexprs).flatten
  | e => [e]

/-- (helper) The number of nodes. -/
def size (e : Expr) : Nat := (subexprs e).length

mutual
/-- (helper) The rule labels one expression exercises, in the seed corpus's
spellings where it has one and in traversal order. `Γ` lists the binder types
innermost first, as `Print.tyOf` reads them, so a use or a `@drop` is labeled
copy or move by the class of the type its place reaches. -/
def rulesIn (D : Decls) (Γ : List Ty) : Expr → List String
  | use pl =>
      (match Γ[pl.root]? with
       | some T =>
           (match T.atPath D pl.path with
            | some T' =>
                (if T'.mult D == .copy then ["(Use-Copy) §5.1"] else ["(Use-Move) §5.1"]) ++
                  (if pl.path.isEmpty then [] else ["§4.2 partial move", "3.8:22"]) ++
                  (if idxStep D T pl.path && T'.mult D != .copy then ["3.8:68"] else [])
            | none => [])
       | none => [])
  | binop op e₁ e₂ =>
      (if op.isCompare then ["(Ord) §5.8", "§6.4"]
       else ["(Arith) §5.8", "§6.4 arithmetic traps"]) ++ rulesIn D Γ e₁ ++ rulesIn D Γ e₂
  | unop op e₁ =>
      (match op with
       | .neg => ["(Neg) §5.8", "§6.4 arithmetic traps"]
       | .not => ["(Not) §5.8"]
       | .bitnot => ["(BitNot) §5.8"]) ++ rulesIn D Γ e₁
  | intCast _ _ e₁ => ["(Int-Cast) §5.8", "(D-Int-Cast-Trap) §6.4"] ++ rulesIn D Γ e₁
  | Expr.panic _ => ["(Panic) §5.8", "(D-Panic) §6.12"]
  | dbg e₁ => ["(Dbg) §5.8"] ++ rulesIn D Γ e₁
  | mkStruct _ args => ["(Struct-Intro) §5.8"] ++ (args.map (rulesIn D Γ)).flatten
  | mkEnum _ _ args =>
      ["(Enum-Intro) §5.5", "(D-Enum-Intro) §6.6"] ++ (args.map (rulesIn D Γ)).flatten
  | .«match» scrut arms =>
      -- The arms are read under their own payload locals, which is what makes
      -- a use of one labeled by the payload's class rather than by whatever
      -- binder happens to sit at that index outside the arm.
      let P : Program := { decls := D, fns := [] }
      let variants := match Print.tyOf P (.int .w64 .signed) Γ scrut with
        | some (.enum e) => ((D.enums[e]?).map EnumDecl.variants).getD []
        | _ => []
      ["(Match) §5.5", "(D-Match) §6.6", "6.3:17", "§5.6 scope exit"] ++
        rulesIn D Γ scrut ++ rulesArms D Γ arms variants
  | drop pl =>
      (match Γ[pl.root]? with
       | some T =>
           (match T.atPath D pl.path with
            | some T' =>
                (if T'.mult D == .copy then ["(@Drop-Copy) §5.3"]
                 else ["(@Drop) §5.3", "§6.11"]) ++
                  (if pl.path.isEmpty then [] else ["§4.2 partial move", "3.8:22"]) ++
                  (if idxStep D T pl.path && T'.mult D != .copy then ["3.8:68", "3.8:73"] else [])
            | none => [])
       | none => [])
  | letIn _ e₁ e₂ =>
      let P : Program := { decls := D, fns := [] }
      ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"] ++ rulesIn D Γ e₁ ++
        rulesIn D ((Print.tyOf P (.int .w64 .signed) Γ e₁).getD (.int .w64 .signed) :: Γ) e₂
  | assign _ e₁ => ["(Assign) §5.2", "§6.8 overwrite-drop"] ++ rulesIn D Γ e₁
  | seq e₁ e₂ => ["(Seq) §5.3", "§6.7 temporary drop"] ++ rulesIn D Γ e₁ ++ rulesIn D Γ e₂
  | .ite c e₁ e₂ =>
      ["(If) §5.5 join"] ++ rulesIn D Γ c ++ rulesIn D Γ e₁ ++ rulesIn D Γ e₂
  | call _ args => ["(Call) §5.8", "(D-Call) §6.9"] ++ (args.map (rulesIn D Γ)).flatten
  | mkArray _ args => ["(Array-Intro) §5.8", "(D-Array) §6.5"] ++ (args.map (rulesIn D Γ)).flatten
  | repeatArray _ e₁ _ => ["(Array-Intro) §5.8", "(D-Array) §6.5", "7.1:38"] ++ rulesIn D Γ e₁
  | indexRead _ idx _ =>
      ["(Use-Untrackable-Dynamic-Copy) §5.1", "(D-Index) §6.5"] ++ (idx.map (rulesIn D Γ)).flatten
  | indexWrite _ idx _ e₁ =>
      ["(Assign) §5.2", "(D-Assign) §6.8", "(D-Index) §6.5", "5.2:14"] ++ rulesIn D Γ e₁ ++
        (idx.map (rulesIn D Γ)).flatten
  | indexDrop _ idx _ =>
      ["(@Drop-Copy) §5.3", "(D-Index) §6.5"] ++ (idx.map (rulesIn D Γ)).flatten
  | ret e₁ => ["(Return-Value) §5.7", "(D-Return) §6.9"] ++ rulesIn D Γ e₁
  | loop e₁ =>
      (if e₁.breaks then ["(Loop-Break) §5.7", "3.8:79", "3.8:80"] else ["(Loop-Div) §5.7"]) ++
        ["(D-Loop-Iter) §6.10"] ++ rulesIn D Γ e₁
  | brk => ["(Break) §5.7", "(D-Break) §6.10"]
  | _ => []

/-- (helper) The labels a `match`'s arms exercise, walked alongside the
declaration's variant list: arm `j` is read under variant `j`'s payload locals
(`armScope`'s order, which is `armCtx`'s). A `match` whose scrutinee
`Print.tyOf` could not type has no variant list, and its arms are then read
under the enclosing binders alone. -/
def rulesArms (D : Decls) (Γ₀ : List Ty) : List Expr → List (List Ty) → List String
  | [], _ => []
  | a :: rest, [] => rulesIn D Γ₀ a ++ rulesArms D Γ₀ rest []
  | a :: rest, Ts :: Tss => rulesIn D (Ts.reverse ++ Γ₀) a ++ rulesArms D Γ₀ rest Tss
end

/-- (helper) The rule labels a program exercises, deduplicated in traversal
order: `rulesIn` under the empty scope of a no-parameter entry point. -/
def rulesOf (D : Decls) (e : Expr) : List String :=
  (rulesIn D [] e).foldl (fun acc l => if acc.contains l then acc else acc ++ [l]) []

/-- (helper) The result type of a generated program: mostly `int`, so the
value line is usually present, with a float often enough that `main` prints a
shortest round-trip rendering (`3.12:40`) as well, and an aggregate often
enough that `main`'s own observation of the value is exercised — for an enum
that is §6.11's tag read, dropping the **active** variant's payload only
(`6.3:20`), or, where `class(E)` is `Linear`, the explicit `@drop`
`Print.observeValue` emits instead. -/
def resultTy (D : Decls) : G Ty := do
  let k ← nat 1 10
  if k ≤ 2 && !D.structs.isEmpty then
    let s ← nat 0 (D.structs.length - 1)
    return .struct s
  if k ≤ 4 && !D.enums.isEmpty then
    let e ← nat 0 (D.enums.length - 1)
    return .enum e
  weighted (← intTy) [(5, ← intTy), (3, ← floatTy), (1, .bool), (1, .unit)]

/-- (helper) One generated case: a declaration environment and a one-function
program whose entry point takes no parameters and returns the drawn type. The
environment is drawn in three rounds — structs, then enums over them, then a
few more structs that may hold an enum in a field — which is the order the
declarations section describes and the reason no draw can build `3.0:5`'s
cycle. -/
def genCase (seed i : Nat) : G Corpus.Case := do
  let nDecls ← weighted 2 [(2, 1), (4, 2), (3, 3)]
  let D₀ ← genEnv 0 nDecls (Decls.ofStructs [])
  let nEnums ← weighted 1 [(2, 0), (4, 1), (3, 2)]
  let D₁ ← genEnums nEnums D₀
  let nHolders ← weighted 0 [(2, 0), (3, 1)]
  let D ← genEnv D₁.enums.length nHolders D₁
  let depth ← weighted 3 [(4, 2), (3, 3)]
  let T ← resultTy D
  let e ← expr D T false true [] T depth
  -- One body in ten ends in a `return` of its value instead, `let r = e;
  -- return r`: a return as the last form of the function body (RUE-2383),
  -- drawn on the side stream (`side`).
  let e ← if ← side (chance 1 10) then pure (letIn false e (ret (use (.var 0)))) else pure e
  return {
    name := s!"gen_{seed}_{i}",
    description := s!"Generated program {i} of seed {seed} ({D.structs.length} struct " ++
      s!"and {D.enums.length} enum declarations, {size e} nodes); " ++
      s!"regenerate with `lake exe ruecore-corpus --gen N --seed {seed}` for any N > {i}.",
    rules := rulesOf D e,
    prog := Program.entry D T e }

/-- (helper) `n` generated cases from `seed`, in order; a pure function of
its arguments. -/
def generate (n seed : Nat) : List Corpus.Case :=
  ((List.range n).mapM (genCase seed)).run' (mkStdGen seed, (stdSplit (mkStdGen seed)).2)

end RueCore.Gen
