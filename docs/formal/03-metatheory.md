# The Rue Core Metatheory

The proofs of `01-core-calculus.md` §7, as they are discharged in the
mechanization (`lean/`, package `RueCore`; ADR-0097). One section per §7
bullet. Each names the Lean theorem that establishes it, the fragment of the
core it covers, and the assumptions it takes. A bullet whose section says
*not yet mechanized* is a promise, not a result; the issue named there tracks
it. This document is filled in by the "Formal core mechanization" project and
is complete when no section says so (RUE-207).

**How to read a theorem here.** The mechanization proves safety over a
definitional interpreter, `eval`, rather than over §6's small-step relation
(ADR-0097, decision 3): a well-typed program evaluates to a well-typed value,
a defined panic, or exhausted fuel, and never to a named stuck state. Each
theorem below is stated in that form. Its agreement with §6's reduction is
the adequacy lemma in the last section, which ADR-0097 requires before any CI
promotion (RUE-2289): soundness is proved (`RueCore.eval_sound`: on a checked
program every value and panic `eval` answers, §6's `→*` reaches), and so is
completeness modulo fuel (`RueCore.eval_complete`: every value and panic §6's
`→*` reaches, `eval` answers at every fuel past the run's length). With both
directions, §7's first bullet is also stated over `Step` itself, in §7's own
terms — preservation in a semantic form; the type-safety section says what
that covers — (`RueCore.step_progress`, `RueCore.step_preservation`,
`RueCore.step_type_safety`). To check any claim yourself:
`scripts/rue lean` builds the package, re-checks it, and prints the axioms
every listed theorem depends on; the reading guide in `lean/README.md` is the
entry point for a reader with no Lean.

**Fragment today.** Scalars — `int(w, s)` at every width `w ∈ {8, 16, 32, 64}`
and both signednesses, `float(w)` at both widths, `bool`, `unit` — and
monomorphic struct types declared by the program — named fields by position,
the `@copy`/`linear` attribute, whether the struct declares a destructor, and
`class(S)` as §3's join of the field classes lifted by that attribute
(`WfStructs` is the equation, and `checkStructs` decides it) — and monomorphic
**enum** types, one payload tuple per variant with `class(E)` the payload join
over every variant (`6.3:19`; `WfEnums` is the equation and `checkEnums`
decides it), the two layers grounded by `3.0:5`'s joint acyclicity (`WfNames`,
decided by `checkNoCycle`, which is what makes either equation a definition —
and which sees **through an array element**, since `3.0:5` names array
elements beside fields and payloads: `struct S { x0: [S; 1] }` is E0483 for
the compiler and fails `RueCore.checkDecls` here); struct literals ((Struct-Intro) §5.8, (D-Struct) §6.5), enum
construction and the `match` that eliminates it in §5.5's canonical form —
one arm per variant, binding that variant's payload as fresh `Owned` locals
that leave scope at the arm's end ((Enum-Intro)/(Match) §5.5,
(D-Enum-Intro)/(D-Match) §6.6, `6.3:17`) — and §6.11's drop order — a value's user
destructor, then its fields in declaration order, recursively, or an enum's
**active** variant's payload only (`6.3:20`); the
fixed-length array `[T; n]` ((Array-Intro) §5.8, (D-Array) §6.5) with `class`
as §3's four-line lift of `class(T)`, §6.11's **ascending** element order, the
surface repeat form `[e; n]` at `7.1:38`'s `Copy` element type, and the
dynamic-index read, write and `Copy` `@drop` — at the element or at any place
below it,
`a[i].x0`, `a[i][j]`, `h.arr[i].x0` — with (D-Index-Trap) §6.5's `bounds`
trap at every dynamic step and a write's right-hand side evaluated before its
indices (`5.2:14`); use
(copy/move), `@drop`, `let` with scope-exit drop, assignment with
reinitialization, sequencing with the discard check, `if` with the §5.5 branch
join, §2's whole integer operator set — `+ - * / %`, `& | ^`, `<< >>`,
`< <= > >=` and the three unary forms, by (Arith)/(Ord)/(Neg)/(Not)/(BitNot)
§5.8 with §6.4's traps and its `val_{w,s}(β_w(·))` bit semantics — `@intCast`
((Int-Cast) §5.8, `(D-Int-Cast-Trap)` §6.4), `@panic` ((Panic) §5.8,
(D-Panic) §6.12) and `@dbg` ((Dbg) §5.8) — whose float rendering is
`3.12:40`–`3.12:42`'s shortest round-trip text; §2's float operator set —
`+ - * /`, `neg`, `< <= > >=` and `@total_cmp`, by
(Float-Arith)/(Float-Neg)/(Float-Ord)/(Total-Cmp) §5.8 with §6.4's trap-free
dynamics — and the one-operand float intrinsics `@int_to_float`,
`@float_to_int` (the one float form that traps, `3.12:18`), `@float_cast` and
the five of `3.12:34`; top-level function definitions,
by-value calls with frames and scope records ((Fn)/(Call) §5.8,
(D-Call)/(D-Return-Value) §6.9), `return` with its σ unwind
((Return-Value) §5.7, (D-Return) §6.9), and `loop` with its nullary `break`
((Break), (Loop-Div-Backedge), (Loop-Div) and (Loop-Break) §5.7, §6.10's
dynamics with `break`'s σ unwind). **Places** are §5's
`Path ::= x | Path.f | Path[c]`, so a use, a `@drop` and an assignment each
name one:
a projection in value context is the partial move of `3.8:22` (§4.2), the
`fully-owned` and `3.9:34` premises of (Use-Move) §5.1 are checked, §5.6's
leak check is the recursive `residual-linear` read on the residue, and §5.5's
join is taken path by path. A path with a declared-`linear` **proper prefix**
takes §4.2's `Declared(d, π_s)` plan instead, and
(Use-Declared-Linear-Destructure) §5.1 discharges it: the smallest enclosing
declared-`linear` place is consumed, §5.1's `linear-residue` gate rejects a
residue that carries a linear value (`3.8:60`), and §6.3's `split`/`drop*`
destroys the droppable residue in the traversal's order before the consumed
place becomes `⊘`. An **array element** is such a path: `3.8:68`'s
constant-index element move, the `MovedOut` element state it leaves and
`3.8:73`'s path-specific element drop are in, bounded by §4.2's "element moves
only at the root" (`RueCore.rootIdxOnly`, E0904) on the two rules that move a
path out and by nothing at all on the declared-linear destructure, which
`3.8:71` admits at an array anywhere in a place tree. Writing into an array
that has a moved-out element is refused as `3.8:72`/`7.1:46` refuse it
(`RueCore.assignArrayOk`, E0480). A place **below** a dynamic index is in
too: §2's place grammar has `p [ e ]`, and `RueCore.Expr.indexRead`/`indexWrite`
(and `indexDrop`, (@Drop-Copy) §5.3's `@drop` of a `Copy` place there, with
the read's premises) carry a constant place `p`, then one or more dynamic steps, each followed by a
constant path of field slots and constant indices. `RueCore.Place` stays
constant-only, so Σ stays finite (`3.8:68`): a dynamic step resolves to a
constant path only at run time (`RueCore.Contents.resolveDyn`). The read wants
a `Copy` leaf, `fully-owned` at `p` and no declared-`linear` proper prefix
anywhere along the path; the write wants `fully-owned` at `p`,
`RueCore.assignArrayOk` above it and a leaf that carries no linear value,
because a place under a runtime index is never `MovedOut` (`3.8:77`). No
equality compare (it borrows its
operands, `4.3:3f`, so `≈`'s float leaf has no instance here), no payload path
into an enum (§5.6 tracks none, so `Place` has no enum step), no wildcard,
repeated or guarded `match` pattern and no bool or integer scrutinee (all
elaboration obligations §5.5 states), no
`inout`/`borrow` parameters, accessor calls, `continue`, loans, or buffers.

**One form with no core image.** §2's elaboration inventory gives the repeat
literal `[e; n]` no core form: it elaborates to `let t = e; [t, …, t]`, one
evaluation of the operand and then `n` value-context copies, which `7.1:38`'s
`Copy` restriction makes free. The mechanization keeps it as a rule
(`RueCore.Typed.repeatArray`) and a machine arm of its own, so that the
printer can emit the surface spelling the compiler's E0905 is about and the
bridge exercises it. Its premise and its dynamics *are* that elaboration's,
but that they are is by construction and not by a theorem: this is the one
place where the mechanization has a form the calculus's core does not. The
machine arm checks the `Copy` premise on the operand's value and refuses a
non-`Copy` one with `typeConfusion`, where the elaboration would be stuck at
the second use of `t`; the dynamic-index read and `@drop` check their leaf the
same way, as (D-Use-Untrackable-Dynamic-Copy) §6.3 requires
(`RueCore.repeatAffine_refused`, `RueCore.dynReadAffine_refused`,
`RueCore.dynDropAffine_refused`). `soundness` discharges all three checks from
the typing rules' own `Copy` premises.

**The trap inventory, and what a trap carries.** Every §6.12 category the
fragment reaches is a `PanicKind`: `overflow` (`+ - *`, `neg`, `min_T / -1`,
`min_T % -1`), `divZero`, `remZero`, `castOverflow` (`@intCast`, `4.13:28`)
and `user` (`@panic`). The one float producer is `@float_to_int`, and it
reaches `overflow` rather than a category of its own (`3.12:18`, `8.1:7`):
float *arithmetic* never traps at all (`3.12:21`). `bounds` is the array
index's, and only a **dynamic** one reaches it: a constant index is
bounds-checked where the place is typed (`7.1:9`, the compiler's E0902), so it
is the one index error that is never a trap. A trap result carries the **observable output that ran
before it** — the user destructors and the `@dbg` lines — because §6.12's
`Outcome` is exit status and stdout together and a trapping process prints
what it printed before exiting 101. `Examples.panicAfterDrop` and
`Examples.dbgBeforeTrap` are the kernel-checked witnesses, one per trap
source, and `crates/rue-oracle-diff` compares that output against the native
binary's. The same section's `panicPastAffine` and `panicPastLinear` are the negative
half: a `@panic` past a live binding carries an **empty** trace, because §5.7
exempts the `⊥_panic` edge from §5.6's obligation and §6.12 abandons the
configuration rather than unwinding it — so a destructor that had not
already run does not run. The linear one is the interesting case, because it
is the only shape where (Panic) and (Return-Value) differ: the linear value
is consumed zero times and no refusal fires, which is the second bullet of
the linear theorem's carve-out below.

**Fuel.** Because a callee's body is not a subexpression of its call,
recursion makes the interpreter's recursion unbounded, so `eval` takes a fuel
bound and reports `outOfFuel` when it runs out. Every theorem below is
quantified over the bound, and two lemmas say that quantification is not
vacuous: `RueCore.fuel_mono` (a result other than `outOfFuel` is the result at
every larger bound) and `RueCore.no_masking` (no bound turns a violation into
exhaustion for a program some fuel completes). `outOfFuel` is the
interpreter's admission that it stopped, not a state of §6's machine, and it
is outside the adequacy correspondence for the same reason.

**Axioms.** Every theorem below depends on `propext` and `Quot.sound` only;
the build fails otherwise (`toolchains/lean/defs.bzl`).

---

## Type safety (progress + preservation)

- **Theorems, over `eval`:** `RueCore.soundness`, and `RueCore.run_safe`
  over a whole program (`lean/RueCore/Soundness.lean`).
- **Theorems, over §6's `Step`:** `RueCore.step_progress`,
  `RueCore.step_preservation`, `RueCore.step_value_typed` and
  `RueCore.step_type_safety` (`lean/RueCore/Adequacy.lean`, RUE-2333),
  derived from the `eval` form and the two adequacy directions (the adequacy
  row below).
- **Statement, in words:** if `Typed P R Γ e T Ω` holds and the frame agrees
  with `Γ`, then at every fuel bound `eval` yields a well-typed value with the
  agreement restored at `Ω`'s normal outgoing state, a value handed back by an
  unwinding `return`, an unwinding `break` that fired at one of `Ω`'s
  delivered states (`RueCore.BrokeOk`), a defined panic, or `outOfFuel`; it
  is never a
  `Violation`. `Ω` is §5.3's outgoing result (`RueCore.Out`): a normal state
  or §5.7's `⊥`, with the edge deliveries. When it is `⊥` the theorem says
  more — a value is impossible, so an expression the rules type as divergent
  never completes normally (`RueCore.EvalOk.bot_abort`), which is what lets
  the `-Bottom` rules type nothing past a diverging subexpression.
  `run_safe` reads it off for a whole program: a well-formed program either
  exhausts its fuel, traps in a defined way, or produces a value of the entry
  point's declared return type. `RueCore.ProgramTyped.run_safe` is the same
  statement from the packaged hypothesis, with the entry function
  existentially quantified (`∃ fd, P[0]? = some fd ∧ …`) because
  `ProgramTyped` only says one exists; the value's type is still that
  function's declared return type. Progress and preservation in one
  statement, because the interpreter is total.
- **Statement over `Step`, in words:** for a program `check` accepts,
  *progress* (`step_progress`): every configuration `→*` reaches from §6.12's
  initial configuration reduces or has halted with a value or a defined panic
  — §7's "does not get stuck" verbatim, so no reduction sequence reaches a
  stuck configuration. *Preservation* (`step_preservation`), in a semantic
  form: every configuration `→*` reaches is `RueCore.Config.SafeAt` the entry
  point's declared return type, which is the program's answer type, not the
  type of the redex in focus. It is `step_progress` and `step_value_typed`
  together, restated as a predicate on configurations. It holds at every type
  on a run that diverges or panics, and its one-step form,
  `RueCore.Config.SafeAt.preservation`, needs no typing hypothesis. *At the
  result*
  (`step_value_typed`): a value §6 halts with has that type. *At every
  horizon* (`step_type_safety`): §6 has run `n` steps, or has halted with a
  well-typed value, or with a defined panic; `RueCore.eval_diverges_iff` is
  the unbounded form of the first case. **What "typed" means here:** the
  configuration typing is semantic, `RueCore.Config.SafeAt` — nothing the
  configuration reaches is stuck, and every value it halts with has the type.
  Its one-step preservation holds by construction, and the content is the
  fundamental lemma `RueCore.init_safeAt`: a checked program's initial
  configuration is typed, which is `run_safe` carried to §6 by
  `eval_complete`. A syntactic configuration typing `⊢ C : T` (a typed store
  and frame stack, one preservation case per `Step` constructor) would be a
  second safety proof over `Step`, not a corollary of this one, and is not
  claimed; the invariant that plays its role is `FrameMatches`, over `eval`.
  The §7 preservation invariant proper, "Σ faithfully tracks the store's
  initialization", is `FrameMatches` over `eval`. No `Step`-side counterpart
  is stated.
- **Invariant:** `RueCore.FrameMatches`, in two halves. `Matches` — "Σ
  faithfully tracks the store's initialization" (§7), whose per-cell clause is
  the **recursive** `RueCore.ContentsMatches`: Σ's state for a binding is a
  tree over its paths (`owned`, `movedOut`, or `fields` for a partially moved
  value) and the cell holds the same shape with §6.1's `⊘` admitted at any
  node, and the two are related path by path. It is deliberately asymmetric at
  the `movedOut` clause: a statically `MovedOut` path may still hold live
  *non-linear* content (the §5.5 conservative join, `3.8:73`), never a live
  linear sub-value (`3.8:50`). Two lemmas turn that into what the proof uses:
  `RueCore.ContentsMatches.residualLinear_false` says the machine's leak
  monitor sees exactly what §5.6's `residual-linear` computes, and
  `RueCore.ContentsMatches.readAt`/`.writeAt` say that navigating a path
  agrees on the two sides wherever Σ has a state for it — which is wherever no
  proper prefix is `MovedOut`, (Owned-Base) §5.1. And the σ invariant: the frame's scope record,
  read newest-first, *is* its environment, which is what lets `Matches` apply
  to `run-all-scope-drops`'s walk. In this fragment that equation is
  **definitional** — every frame the interpreter builds builds σ and ρ from
  one list — so it is not yet evidence about the RUE-1277 redundancy, which
  was raised for scopes pushed and popped independently of the binder chain.
  §6.6's `match` arm **appends** its payload cells to the innermost record, as
  (D-Let) §6.7 does, and so does a loop body: §6.10's `push-scope` is read as
  the record's length at the loop's entry, and a `break` hands the loop the
  record of the frame it fired in (`RueCore.EvalRes.broke`), whose cells past
  that length are exactly the body's still-open bindings. So the equation
  stays definitional, and a `break`'s unwind is `RueCore.Matches.unwindPrefix`
  on those cells (`RueCore.loop_exit_ok`). It becomes a real obligation when
  `Frame.scope` is §6.1's stack. `RueCore.Untouched` carries frame locality
  across a call, so a caller's agreement survives a callee's run.
- **Hypothesis:** `RueCore.ProgramTyped` — §3's class assignment for every
  struct **and enum** declaration together with `3.0:5`'s acyclicity
  (`RueCore.WfDecls`) and (Fn) §5.8 for every function, plus an entry point
  taking no parameters — which `RueCore.checkProgram_sound` decides.
- **Covers:** the fragment above. **Owed:** every remaining Phase C slice
  re-establishes this theorem for its forms (RUE-2233 through RUE-2237). The
  array slice (RUE-2235) has done so for all of its forms — `[T; n]` and its
  literals (RUE-2322), the constant-index element move and its `MovedOut`
  element state (RUE-2327), and places below a dynamic index (RUE-2342) — and
  it also widened `3.0:5`'s own relation: `RueCore.Decls.Names` reaches a
  declaration through `RueCore.Ty.declIds`, which peels array wrappers, so a
  struct that names itself through an array element is refused rather than
  grounded. RUE-2331 then put the array forms under the generator, and at
  `--gen 200 --seed 7` and `--gen 1000 --seed 23` the model's verdicts and
  traces agree with the compiler's on every generated case but two shapes,
  neither of them the theorem's: the self-assignment `a[c] = a[c]` (refused by
  `3.8:72` as §5.2 orders it, accepted by the compiler since RUE-228 — a
  reading still to decide, RUE-2346; one case at seed 7, two at seed 23 and a
  third there that the compiler's E0904 masks) and a dynamic index into a
  zero-length array field (an internal compiler error where the model traps
  with `bounds`, RUE-2345, fixed since; five cases at seed 23). Both are
  seeded; the second agrees now that RUE-2345 is fixed. Those
  two settings reached nothing else; wider runs at other seeds reach RUE-2344's
  read below a dynamic index after its array moved (`gen_2_1694`,
  `--gen 1695 --seed 2`) and two compiler defects filed from them, RUE-2347
  and RUE-2348. The
  enum slice (RUE-2320) has done so: progress at a `match` is exhaustiveness
  (`RueCore.exhaustive_arm_exists` — a well-typed tag is an index the arm list
  has), and preservation over the n-way join is the fold of the binary one
  (`RueCore.Matches.joinAll`); RUE-2325 then put the enum forms under the
  generator, and at `--gen 200 --seed 7` and `--gen 1000 --seed 23` the
  model's verdicts and traces were compared against the compiler by hand and
  agree. That is a differential check of `check`/`eval` against the
  implementation, not part of the theorem, and nothing in CI runs it yet
  (RUE-2241). The float slice (RUE-2282) has done so: `soundness` is stated
  over a `RueCore.FloatModel`, so the float forms carry their own
  re-establishment in the theorem's statement. The loop slice (RUE-2234) has
  done so: preservation across the back edge is the loop-head equation
  (`RueCore.LoopHead.reenter`, resting on `RueCore.Ctx.join_absorb`), a
  `break`'s exit is `RueCore.loop_exit_ok`, and nontermination is fuel's.
  RUE-2330 then put loops under the generator, which changed the programs both
  settings produce; on those draws four cases reach the RUE-2346
  self-assignment (`gen_7_145`, `gen_7_159`, `gen_7_181`, `gen_23_752`), each
  masked by an E0406 the compiler reports first, so every one of the 1,200
  agrees with the compiler. A wider run reaches it unmasked (`gen_101_207`,
  `--gen 400 --seed 101`), an allowed RUE-2346 disagreement.

## No use-after-move

- **Theorem:** `RueCore.no_use_after_move`.
- **In words:** no evaluation of a well-typed program touches a `⊘` — at the
  root of a cell, or at any node inside it.
- **Covers:** whole bindings **and paths**. The premise that carries the
  second is `fully-owned(Σ, p)` (§5.1, `3.8:26`): a read that reaches a hole
  anywhere inside the aggregate it names is `useAfterMove`, and the rule
  forbids handing such an aggregate to a new owner — which is the compiler's
  E0205 "use of partially moved value" (`RueCore.Examples.partialThenWhole`).
  Reading a *sibling* through a partially moved base stays legal
  (`3.8:53`, (Owned-Base) §5.1, mechanized as `RueCore.OwnSt.get`). An array
  element at a constant index is a path like any other, so a read of one is
  covered; a *dynamic* index cannot be tracked as a path at all, and
  §5.1's (Use-Untrackable-Dynamic-Copy) asks `fully-owned(Σ, p)` of the whole
  array for exactly that reason (`3.8:70`, `7.1:45`). A place **below** a
  dynamic index reads the same way: the premise is at the array `p` the first
  dynamic step indexes, so every element under it and every place below one is
  hole-free, and each dynamic step either traps on its bound or lands on an
  element the invariant covers (`RueCore.Contents.resolveDyn_ok`). It is `p`,
  not the root: `a[0][i].x1` after a move of `a[1]` reads a whole `a[0]`, and
  the compiler accepts it too (`RueCore.Examples.dynReadAfterSiblingMove`).
  A declared-linear destructure reads the same way, one place up: the rule's
  `fully-owned(Σ, d)` is asked of the **consumed** place, so the leaf it hands
  on and the residue it destroys are both hole-free
  (`RueCore.splitResidue_ok`).
  The constant-index element **move** is in, with the `MovedOut` element state
  it leaves (`RueCore.Examples.arrayElemMove`), so a use of the array as a
  whole or a read through a moved element is `fully-owned`/(Owned-Base) again
  (`3.8:70`, `7.1:45`, E0205).

## No double-free

- **Theorem:** `RueCore.no_double_free` (`lean/RueCore/Trace.lean`).
- **In words:** a well-typed program's run is never refused, and in its
  trace no identity has its destructor run twice, and no identity appears
  twice among the `drop`/`dropTemp` free events.
- **What is counted.** §6.1 gives values no identity, so the machine adds
  one (RUE-2323): aggregate introduction — (D-Struct) and (D-Array) §6.5,
  (D-Enum-Intro) §6.6, the repeat form — **mints** a value identity, the
  store's next index, reserved with `†` (`RueCore.introVal`; §6.1's "a fresh
  identity is one not in `dom(H)`"). The identity travels with the value
  through every move and binding, and the trace's `drop`, `dropTemp` and
  `dtor` events carry the values they ran on, identity included. The theorem
  counts two projections of the trace:
  - `RueCore.freedIds`: the owned identities each `drop`/`dropTemp` marker
    frees, meaning the non-`Copy` nodes of the dropped tree, `⊘` skipped;
  - `RueCore.dtorIds`: the identity of each value a user destructor ran on.
    This is §7's own wording, "every stored value's destructor runs at most
    once".

  Each identity occurs at most once in each (`List.count a ≤ 1`). A `Copy`
  value is duplicated freely and has no drop glue, so its copies share an
  identity and neither projection counts them. A declared-linear
  destructure's residue drops each retained subtree under a `drop` marker of
  its own (`RueCore.residueMark`), and the shell a `match` or a destructure
  consumes is ended by a `consume` event (RUE-2427), so `freedIds` counts
  both.
- **Proof.** `RueCore.eval_conserves` is a conservation law, proved by fuel
  induction over `eval`, one case per form. The owned identities of the final
  store, of the result value and of the trace's projection together are, as
  a multiset, at most those of the initial store plus the ones minted during
  the run. Minting takes store indices, so the minted ones form a range and
  appear once each. §7's "Because" is the law's cases:
  - a move writes `⊘` where it took (§6.3);
  - §6.11's walk skips every `⊘` ("this single skip is what makes
    double-free impossible");
  - the declared-linear rule hands on its leaf, drops the residue once and
    leaves `⊘`;
  - a `match` moves the payload whole into the arm's cells, so the
    scrutinee's owner is gone.
- **What the law needs**:
  - **Copy closure** (`RueCore.Contents.copyClosed`): nothing owned under a
    `Copy` node, or a copy would duplicate it. §3 makes this a property of
    every well-typed value (`RueCore.ContentsTy.copyClosed`). The machine
    also enforces it with a fourth monitor beside the linear three,
    `ownedUnderCopy`, at aggregate introduction and at an assignment. The law
    therefore holds for every finished run of any program with well-formed
    declarations: `RueCore.freed_once`, and `RueCore.dtor_once`, which uses
    `3.9:31`, "a destructor-bearing struct is not `Copy`".
  - **Typing**, which enters through `RueCore.no_violation`. A checked
    program never reaches the monitor or any other refusal, so its trace is
    the whole run's. A refused run carries no trace, so without this conjunct
    the statement could be satisfied vacuously.
- **The monitor is load-bearing, not decorative.** An ill-typed program
  puts an `S1` with a destructor into an `@copy` struct's field, copies the
  struct and drops the field through both copies:
  - `check` rejects it;
  - `eval` refuses it with `ownedUnderCopy`;
  - §6's relation, `Step`, has no monitor and runs `S1`'s destructor on the
    same identity twice (`RueCore.dupProgram_step_double_free`).
- **Covers:** the whole fragment, for by-value programs: scalars, structs,
  enums, arrays, places with constant and dynamic indices, calls, `return`,
  loops and `break`. Buffer cells (§6.13.3) are not in the fragment.
- **The walk's order**, alongside: the events a drop emits have a proved
  closed form, **over cell contents rather than values** — which is where the
  `⊘`-skip that makes double frees impossible lives.
  `RueCore.dropContents_struct_events` (`lean/RueCore/Soundness.lean`) says
  that for well-typed contents at a struct type the events its drop emits are
  its user destructor's event — when the declaration has one (`3.9:28`) —
  followed by the concatenation of its fields' drop events in **declaration
  order** (`3.9:13`), each field's given by the same closed form recursively
  and a field that has been **moved out contributing none** (`3.8:73`);
  `RueCore.dropContents_enum_events` is the same reading at an enum — the
  **active** variant's payload only, in payload order, no destructor event
  because §3 lets an enum declare none, and nothing at all for a
  discriminant-only variant (`6.3:20`);
  `RueCore.dropContents_events` is the equation it reads off, and
  `RueCore.dropEvents` (`lean/RueCore/Dynamics.lean`) is §6.11's order written
  as a function. `RueCore.dropContents_array_events` is the same closed form
  at an array type: no destructor event of the array's own (`3.9:14` gives
  `[T; n]` a destructor exactly when `T` has one) and the elements'
  events concatenated in **ascending index order** (`3.9:15`, `3.8:73`). `RueCore.dropContents_ok` says the walk never refuses on
  well-typed contents, and
  `RueCore.ContentsMatches.residualLinear_false` says contents the leak
  monitor lets through holds no live declared-`linear` sub-value — the
  residual reading §5.6 asks for, which is what makes a partially consumed
  carrier's residue droppable. "Exactly once" (that every owned, droppable,
  non-moved value *is* dropped) is `RueCore.drop_exactly_once`, in the next
  section; this section is the "at most once" half.

## No use-after-drop / no leak of drops

- **Theorem:** `RueCore.no_use_after_drop` — the "never read afterward"
  half: no evaluation touches a retired (`†`) cell. With frames this is a
  consequence of the invariant rather than a structural fact about closed
  expressions: `run-all-scope-drops` (§6.9) walks the frame's scope record at
  every `return` and at every frame pop, and it is `FrameMatches` — the record
  *is* the environment, whose cells `Matches` says are live or moved out and
  pairwise distinct — that keeps those walks off a `†` cell and stops any cell
  being retired twice. The guard is also witnessed directly from an open
  machine state, including one whose scope record names a retired cell
  (`Examples.lean`).
- **Theorems:** `RueCore.drop_exactly_once` and `RueCore.rest_exactly_once`
  (`lean/RueCore/TraceExact.lean`, RUE-2427): the "exactly once" half.
- **In words.** Take a well-typed configuration of a checked program: an
  expression typed in `Γ`, run in a frame and store that agree with `Γ`. Its
  evaluation is never refused, and when it ends normally or unwinds by
  `return` or `break`, two things hold.
  - **The ledger.** Every owned value the store held when the evaluation
    started is in exactly one place: in a cell that already existed, part of
    the result, or ended in the trace exactly as many times as it was held
    (`RueCore.Exact`).
  - **The frame-pop invariant.** Every cell the evaluation allocated is
    retired: a `let`'s at its `endscope`, a `match` arm's at the arm's end,
    a callee's at its frame pop. The one exception is an unwinding `break`,
    which leaves the cells its scope record still owes. The loop retires
    them, and `rest_exactly_once` at the loop counts their values as ended
    (see below). Cells outside the frame's environment were touched only to be
    retired, and an unwinding `return` has retired the frame's whole record
    (`RueCore.Tidy`, `RueCore.eval_tidy`). So "still in the store" never
    means a cell nobody can reach any more. `RueCore.orphan_rejected` is a
    frame pop that forgot its σ-walk: the bare ledger balances and `Tidy`
    rejects it.

  So a binding's value is dropped on the normal path (the binding's
  `endscope`, §6.7) or on the unwind path (the σ-walk, §6.9 and §6.10),
  never both and never neither. "Exactly once" presumes each starting
  identity is held once, which every store a run reaches satisfies
  (`no_double_free`). The equation counts multiplicity in general.
- **What "ended" means.** `RueCore.freedIds` reads the trace's markers, and
  every way an owned value's life ends has one:
  - a binding's drop, `drop ℓ c` — at scope exit, `@drop`, an overwrite, and
    each retained subtree of a declared-linear destructure's residue;
  - a discarded temporary, `dropTemp v`;
  - a **consumption**, `consume c`: the shell a `match` leaves once its
    payload is bound (§6.6), and the path from `d` to the leaf a
    destructure leaves once the leaf is handed on and the residue dropped
    (§6.3). Neither runs a drop of its own, because every member has already
    gone somewhere, but the value's life ends there. Without the event the
    count could only be an inequality.

  All three are markers no Rue program observes, so the corpus and the
  bridge are unchanged.
- **Why two statements, not one over `run`.** A run starts from the empty
  store, so every identity it mentions is minted during it, and its result
  does not say which store indices were minted for owned values.
  `drop_exactly_once` counts the identities an evaluation *starts* with
  (`a < H.length`). A value an operand mints and its own form ends is held
  by no evaluation at its start: `let x = S { .. }`, `S { .. };`,
  `g(S { .. })`, and a scrutinee's shell are all examples.
  `RueCore.rest_exactly_once` covers those. Once a form's leading operands
  (its first operand, or the argument list of a call, a literal or a dynamic
  read) have produced their values, whatever the rest of the form yields is
  not refused, keeps the same ledger with those values held, and has
  retired every cell allocated since (`RueCore.Lead`, `RueCore.rest_step`,
  `RueCore.Settled`). A `loop`'s lead is its body **breaking**, so the
  `break`'s unwind of the bindings the body still held is a rest too.
  Three witnesses are each stated at a typed configuration of a checked,
  `pendingSafe` program, and in each the real run satisfies the theorem:
  - `RueCore.letDropDeleted_rejected` deletes the `let`'s drop;
  - `RueCore.seqDropDeleted_rejected` deletes the discard's drop;
  - `RueCore.breakLeak_rejected` has a loop retire a body-minted binding's
    cell without dropping it.

  The bare ledger accepts all three (with `Tidy`, for the loop), and
  `rest_exactly_once`'s ledger rejects them. Every place the machine ends
  an owned value lies inside the window of a statement that already counts
  the value: the rest of the form that bound or received it, the rest of
  the loop a `break` unwinds to, or an evaluation that started holding it.
  The places are an `endscope`, a discard, a frame pop, a `return`'s σ-walk,
  a `break`'s unwind, a consumption, an overwrite and `@drop`. So every
  owned value a checked run holds ends exactly once by the end of the
  window that holds it, and is never left in a cell nobody can reach.
  What neither theorem sees is an end emitted *early*, inside the
  evaluation that minted the value: no window holds the value yet, so only
  `no_double_free` bounds it (at most once). Nor does either see `main`'s
  own result, which is part of `run`'s result and handed to no form.
- **Carve-outs, stated where they apply:**
  - **`@panic`** (§6.12): a trap carries no store and runs no drop, so a
    `panic` result carries no claim. The values live at the trap are
    abandoned, which is §5.7's `⊥_panic` edge.
  - **RUE-2316**: a value computed for an earlier operand that a later
    operand abandons by `return` or `break` is dropped by nothing, in the
    calculus as in the compiler. The hypothesis `RueCore.Program.pendingSafe`
    / `RueCore.Expr.pendingSafe` excludes the shape syntactically: no operand
    after the first of a call, a literal, an index list or a binary
    operator, and no index of an indexed assignment, may contain a `return`
    or a `break` its own loops do not catch. It is a whole-program
    hypothesis, and every seed the checker accepts satisfies it (a `#guard`
    in `TraceExact.lean`). `RueCore.pendingSafe_needed` shows it is load-bearing: at a
    typed configuration of a program the checker accepts, `x`'s value is in
    neither the store, the result nor the trace once the sibling's `return`
    has unwound.
- **Proof.** `RueCore.eval_exact` is `eval_conserves`' conservation law read
  as an equality over the starting identities, by the same fuel induction.
  Like it, it needs no typing derivation: copy closure, which the machine
  maintains, closes every case. So do two shape premises, in `eval` and
  `Step` alike: `@dbg`'s operand is observable, the only values §6.12
  renders, and a loop body's value is `⟨⟩` (§6.10). Otherwise a value would
  vanish unrecorded. Typing enters through `soundness` only: a checked
  configuration is never refused. `RueCore.eval_quiet` is the syntactic
  side: an expression with no `return` never unwinds by one, and one with no
  free `break` never by that. The induction step is `RueCore.rest_step`
  followed by the operand's own ledger, so the continuation ledger that
  `rest_exactly_once` states is the one the proof checks. `RueCore.eval_tidy`
  is a separate fuel induction on the store's shape.
- **Owed:** drop *order*, meaning when and in what order the drops run
  (reverse registration at scope exit, §3.9 and §6.11 within a value), is
  `drop_order`, part 2b of RUE-2328 (RUE-2428). These theorems say that
  every owned value ends exactly once, not when.

## No use-after-free

- **Not yet mechanized.** This is the §6.13 buffer bullet; it needs the
  allocation store and the §6.13.5 obligations as explicit interfaces
  (RUE-2240), after loans (RUE-2238).

## Linear values are consumed exactly once

- **Theorems:** `RueCore.no_linear_leak` (§5.6 scope exit, and §6.9's frame
  teardown at an early `return` or a frame pop),
  `RueCore.no_linear_overwrite` (§5.2, `3.8:77`),
  `RueCore.no_linear_discard` (§5.3, `3.8:64`).
- **In words:** a well-formed program never reaches the refusal the machine
  raises when a linear value would be leaked, overwritten while live, or
  discarded. The leak half covers three edges: a `let`'s scope exit, a frame's
  normal pop (a by-value parameter the callee never consumed — (Fn) §5.8's
  second clause, `3.8:62`), and a `return`'s `⊥_exit` unwind. The frame-pop
  edge is reached only through `Examples.lean`'s kernel-checked
  `run linearParamLeaked … = .stuck .linearLeak`: the bridge cannot exercise
  it, because the compiler rejects that program (E0406) before anything runs.
- **Carve-out, the two edges no refusal covers.** On both a **linear value
  can be consumed zero times** with none of the three refusals firing, and a
  destructor-bearing one loses its observable drop silently. They are
  different in kind, and only the first is a gap.
  - **A pending value (open, RUE-2316).** A value already built for a
    **sibling position** — a call's argument, a struct literal's initializer,
    an array literal's element, and an assignment's right-hand side while the
    target's indices run after it (`5.2:14`) — is in no cell and no scope
    record between the subexpression that produced it and the aggregation that would have taken
    it (§6.9's `mintParams` for an argument). If a *later* sibling
    unwinds by `return` or `break`, (D-Return) §6.9 or (D-Break) §6.10
    discards the evaluation context with the pending values in it and
    unwinds only σ, so that value's drop is neither run nor monitored. This
    is the calculus as written — (D-Return) and (D-Break) unwind σ and
    nothing else, and (Strict-Bottom) §5.7, the only bottom
    rule for an argument position, imposes no §5.3 discard check on siblings
    already evaluated — so the statics cannot reject it without ⊥ provenance
    they do not carry, and the Rue compiler behaves the same way (the
    destructor does not run). Probe r14 of RUE-2342,
    `a[if c { return 5 } else { 0 }].s = mk(9)`, printing `9` and never
    running the new `S1`'s destructor, is RUE-2316's pending-value edge at the
    assignment's right-hand side, where only the affine half applies: (Assign)'s
    leaf premise `class(T) ≠ Linear` (`3.8:77`) keeps the abandoned value from
    being linear. The mechanization models the calculus rather
    than patching it and states the gap instead: `Dynamics.lean`'s "Pending
    values" section, the `no_violation` docstring, and the kernel-checked
    witnesses `RueCore.Examples.linearLostAtCallArg`,
    `affineLostAtCallArg`, `linearLostAtArrayElem` — at an array element
    rather than an argument — and `linearLostAtBreakArg`, the same loss by a
    `break` (the RUE-2369 review's probe q30, which the compiler matches),
    all of which `checkProgram` accepts and
    all of which end with an empty drop trace. Closing it needs a rule, in
    §5.7, §6.9 or §6.10, and is tracked as RUE-2316. A `@panic` *sibling* of a
    pending value reaches the identical state, by this route as well as the
    next.
  - **A `@panic` (by design).** §6.12 abandons the configuration and §5.7
    exempts the `⊥_panic` edge from §5.6's obligation, so a trap runs no
    scope drop at all — where a `return` in the same position would have
    unwound the frame and run every one. A live linear binding at a `@panic`
    is therefore destroyed with no violation and an empty trace. That is
    (Panic) as specified rather than a gap, and the Rue compiler agrees:
    `RueCore.Examples.panicPastLinear` is the program, with its `Typed`
    derivation (`panicPastLinear_typed`), `checkProgram = false`, and
    `run … = .panic .user []` all kernel-checked; the compiled program prints
    nothing, says `panic: boom` and exits 101 (verified by hand).
    `panicPastAffine` is the same shape one class down, where the obligation
    was never there to lose.
- **Covers:** whole bindings, **paths and per-field obligations**, the binary
  join, by-value parameters, and struct values whose class is `Linear` through
  a field — §3's join, proved to be what a declaration records
  (`RueCore.class_unique`, one unconditional statement over both layers, with
  `RueCore.struct_class_unique` its struct projection) and to reach `Linear`
  exactly when the declaration says so or a field does (`RueCore.struct_carriesLinear_iff`,
  §5.3's `carries_linear` lifting) — on every edge but the two above. The
  per-field half is §5.6's `residual-linear` read on the residue
  (`RueCore.residualLinear`), so consuming exactly the linear part of an
  infectious carrier and letting the rest drop is accepted (the RUE-1591
  model), while stranding a linear sub-place under a partially moved place is
  rejected ((@Drop) §5.3's own side condition, E0406). An array node carries no
  obligation of its own — it declares no attribute, and `3.8:74` makes a
  zero-length one vacuous — so `residual-linear` reads it as the disjunction
  over its `n` elements at the element type, which is what makes an `[L; n]`
  left to scope exit the leak E0406 reports. A **declared-linear destructure**
  consumes the smallest enclosing declared-`linear` place once and destroys its
  droppable residue exactly once, in the traversal's order
  (`RueCore.dropResidue_events`); §5.1's `linear-residue` premise is what
  keeps a *linear* residue out of that destruction, and the machine's own
  residue monitor makes the excluded state a named refusal rather than a
  silent drop. A declared-linear **ancestor** keeps its own obligation, which
  §5.6's declared clause checks and which (@Drop) discharges once no
  still-owned linear sub-place remains below it (the corpus case
  `destructure_ancestor_dropped`, red until RUE-2335 was fixed). For an **enum** the obligation is the
  type's, over every variant (`6.3:19`, `RueCore.enum_carriesLinear_iff`),
  because the active variant is not a static fact: a value of the other
  variant is still must-consume, which is what the compiler reports as
  E0406. A `match` discharges it by binding and consuming the payload, and
  the arm's own §5.6 check is what makes "consuming" mean it (`6.3:17`).
  An **array of linear elements** is consumed element-wise (`3.8:71`): each
  element move discharges that element's share, the §5.5 join refuses where an
  element is consumed on one path only (E0443), and §5.6's element-wise reading
  is what reports the ones left over (E0406). §5.6's second disjunct for an
  array, "(untracked residue carries linear)", needs nothing of its own: the
  calculus has no dynamic-index move — a move or a `@drop` of an affine or
  linear place below a dynamic index, and a use below a dynamic index under a
  declared-`linear` prefix, are refused (E0904), while a `@drop` of a `Copy`
  place there (`RueCore.Expr.indexDrop`) moves nothing — so the elements Σ has
  no record for are `Owned`, and `residualLinearFields`' `[], Ts` base case
  answers them at the element type **exactly**. A write below a dynamic index
  is refused wherever its leaf carries a linear value (`3.8:77`, E0493), since
  a place under a runtime index is never `MovedOut`. **Owed:** RUE-2316.

## Exclusivity / no aliased mutation

- **Not yet mechanized.** Λ is ambiently empty in this fragment; the
  theorem and §7's four owed lemmas (loan/drop non-interference, loan-extent
  nesting, root separation, view-intact) are RUE-2238.

## Lemmas §7 owes, and the metatheory's own

| Lemma | Status |
|---|---|
| Totality of the float operations | **assumed, named** — and less of it assumed than §7 expected. The obligation splits three ways. (1) *Totality as a function* is free: every §6.4 float operation is a total Lean function, so no float redex is stuck for want of a result. (2) *The `(D-Float-To-Int)`/`(D-Float-To-Int-Trap)` partition* is a **theorem**, `RueCore.floatToInt_partition`, because §2's datum model makes truncation exact integer arithmetic; `RueCore.evalFintrin_float_res` is it at the machine, and `RueCore.binOpFloat_res` is the matching statement for the arithmetic and the compares — the latter with no trap disjunct at all (`3.12:21`). Closure of the *exact* operations in `𝔽_w` is proved too: `RueCore.negate_wf` (`(D-Float-Neg)`), `RueCore.widen_wf` (the widening half of `(D-Float-Cast)`) and `RueCore.roundOp_wf` (`@floor`/`@ceil`/`@trunc`/`@round`). (3) What is **assumed** is closure of the *rounded* operations, which is IEEE's and not the mechanization's: the fields `arith_wf`, `sqrt_wf`, `ofLit_wf`, `ofInt_wf` and `narrow_wf` of `RueCore.FloatModel`, together with the behavioural laws §6.4 quotes from `3.12:22` and `3.12:19` (`arith_nan`, `narrow_nan`, `div_by_zero`, `zero_div_zero`) and `3.12:9`'s `ofLit_zero`/`ofLit_one`. Every one of those is a statement that is true of IEEE 754 *and* of the compiler, which is why the two NaN laws are the **weak** ones — a NaN operand yields *a* NaN, sign unspecified. `3.12:44`'s `σ_NaN` is the sign of a NaN an invalid operation *creates*; a propagated NaN keeps its operand's sign on every target Rue has, so a law that fixed the result's sign would be false of both. What `RueCore.Float.exactOps` propagates (the first NaN operand's sign) and the `σ_NaN` it picks are therefore **model choices**, checked against the compiler rather than assumed, and no theorem depends on either. They are structure fields, not `axiom` declarations, so every theorem that uses one carries it in its statement and `TRUST.md` lists them apart from Lean's axioms (`lean/RueCore/Float.lean`) |
| Totality of the **integer** operations | discharged where it is needed, by construction rather than as a lemma: `RueCore.valOf_inBounds` says every `w`-bit pattern read at a signedness denotes a value of that type, which is what makes §6.4's bitwise and shift rules total, and `RueCore.binOpInt_res`, `RueCore.evalUnOp_int_res` and `RueCore.evalIntCast_res` say every integer operator lands on a value of its rule's type or on a trap — the operator half of progress. The last two name the category: `neg` traps only as `overflow` and `@intCast` only as `castOverflow`. `binOpInt_res` ranges over §6.12's categories rather than naming one, because `/` and `%` add their own (`lean/RueCore/Soundness.lean`) |
| Handle-uniqueness preservation (O1) | not yet mechanized (RUE-2240) |
| §6's reduction relation, mechanized | **defined; `eval` proved adequate to it in both directions** (next row). `RueCore.Step` is §6's `C → C'` over the §6.1 configuration for the fragment, one constructor per §6 rule, each citing it; §6.2's evaluation contexts are a stack of frames (`RueCore.Kont`), so (Search) is an enter and a plug constructor per context production and (Panic-Lift) is the shape of every trap rule. Proved about it here: it is deterministic (`RueCore.Step.det`), a terminal configuration takes no step (`RueCore.Step.terminal`), and every configuration is terminal, steps, or is stuck (`RueCore.Config.trichotomy`, through the function `RueCore.step` and `RueCore.step_iff`), with every stuck state named by one of §6's own four violations and never by one of `eval`'s four monitors (`RueCore.step_stuck_isStuckState`). `RueCore.Config.stuck_iff` states stuckness in `Step`'s own terms: not terminal and no step. `RueCore.unwindLocs_plain` and `RueCore.destructure_plain` say a monitor only removes behaviour. Where it departs from §6's text: (Search) is an enter and a plug constructor per context production; the `endscope` frame pops its cells by count, because bindings are de Bruijn indices where §6.7 relies on α-renaming; the loop boundary sits above its context's frames, because §6.10's `loopβ(e, φ)` records no context; `push-scope` is one scope record read by length, so (D-Loop-Iter) and (D-Break) drop the cells past the loop's record; the use plan is recovered from the store rather than read off `μ`; a destructor is one trace event rather than a nested run; aggregate introduction mints a value identity by reserving a `†` store slot, exactly as `eval` does, so the two presentations keep one store (RUE-2323); a `match` and a destructure record the shells they consume with a `consume` event, and the destructure drops each retained residue subtree under a `drop` marker, as `eval` does (RUE-2427); `RueCore.Config.init` calls the entry point, so (D-Return-Main) is (D-Return) reaching its `call` frame; a dynamic read, `@drop` at a dynamic place, or repeat operand that is not `Copy` is stuck, the premise of the rule each cites; and so, in both presentations, are a `@dbg` operand §6.12 cannot render and a loop body value that is not `⟨⟩` (§6.10), which §6 would otherwise discard without a drop (RUE-2427). On programs `check` rejects, `Step` follows §6 where `eval` does not: `@drop` of a `⊘` place is §6.11's no-op where `eval` refuses it, and an owned value put under a `Copy` node steps where `eval` refuses it with `ownedUnderCopy` (`lean/RueCore/Step.lean`) |
| Adequacy of `eval` to §6's reduction, and progress/preservation derived over the mechanized relation | **proved (both directions), with progress and preservation derived over `Step`**. Its domain is the programs `check` accepts: there `eval`'s `ok`/`panic` outcomes must agree with §6's values and panics, and neither side gets stuck. `.stuck` is outside the correspondence, because four of `eval`'s refusals are monitors §6 does not have, and because `eval` names an operand-shape mismatch where §6 simply has no rule (`Dynamics.lean`, "the correspondence with §6"). A raw literal outside its type's `n_T` range is the other known difference, and `check` rejects it. **Soundness** is `RueCore.eval_sound` (`lean/RueCore/Adequacy.lean`, ADR-0097 decision 3, §6.2): for a checked program, `run` is never `.stuck` (`RueCore.no_violation`); a value it answers is reached by `→*` from §6.12's initial configuration as a terminal configuration with the same store and trace, and a panic as `↯κ` after the same trace. It rests on `RueCore.eval_sim`, a simulation `RueCore.Sim` between each `eval` outcome and `→*` from the expression in focus under any context: a value reaches the hole's value in the same frame ((Search) §6.2), a panic reaches `↯κ` ((Panic-Lift) §6.2), an unwinding `return` the nearest caller ((D-Return) §6.9), an unwinding `break` the nearest loop's context ((D-Break) §6.10). The simulation holds on every program (`RueCore.run_sim`), with no typing hypothesis, because every place `eval` and `Step` differ is a refusal on `eval`'s side; typing only fixes the domain. **Completeness modulo fuel** is `RueCore.eval_complete` (RUE-2332, ADR-0097 decision 3, §6.2, §6.12). For a checked program, if `→*` takes the initial configuration to a terminal configuration, `✓` or `↯κ`, in `n` steps, then at every fuel past `n` `run` answers that value or panic with the same store and trace. It rests on one fact, `RueCore.eval_steps_of_outOfFuel`, which holds on every program: if `eval` exhausts `fuel`, `Step` has a run of `fuel` steps from the same expression in focus. With determinism (`RueCore.StepsN.bound`, `RueCore.Steps.final_unique`), `run` cannot be out of fuel past the length of a run that ends, and `run_sim` places whatever it answers at that end. Off the checked domain the same argument gives `RueCore.run_complete`, up to a refusal of `eval`'s, and `RueCore.dropMoved_refused` shows that the refusal really occurs. **"Never stuck", both ways**, is `RueCore.never_stuck_iff`. For a checked program, "at every fuel `run` is never `.stuck`" is equivalent to "every configuration `→*` reaches from the initial one reduces or has halted", which is §7's phrasing. The forward direction holds on every program (`RueCore.step_never_stuck_of_run`). The backward one is `no_violation` on the checked domain, and fails off it (RUE-2314). **Divergence** is `RueCore.eval_diverges_iff`: on a checked program, `run` is out of fuel at every fuel exactly when §6 has runs of every length from the initial configuration, which by determinism is one infinite run. So `outOfFuel` never hides an answer §6 reaches. `fuel_mono` and `no_masking` state the same stability from `eval`'s side. **Progress and preservation over `Step`** (RUE-2333) are `RueCore.step_progress`, `RueCore.step_preservation` and `RueCore.step_type_safety`, stated in §7's terms and derived from `soundness` and these two directions (the type-safety section, which also says why preservation is stated for a semantic configuration typing). `RueCore.affineScopeDrop_both_ways` is one corpus program, `affine_scope_drop`, in both presentations: `check` accepts it, `run` answers it, and a twelve-step `Step` derivation, written out constructor by constructor, reaches the same end (`lean/GUIDE.md`, "One program, traced both ways") |
| §5.5's `join(Σ1, …, Σn)` read unordered and unbracketed | **proved**, over states that are shapes of their declared types. The binary join is commutative (`RueCore.OwnSt.join_comm`, `RueCore.Ctx.join_comm`) and associative (`RueCore.OwnSt.join_assoc`, `RueCore.Ctx.join_assoc`), so `RueCore.Ctx.joinAll_perm` says the left fold the mechanization computes is invariant under a permutation of the arms — which is what licenses reading `RueCore.Ctx.joinAll` as the unordered n-way join §5.5 writes. Associativity carries two premises and `lean/RueCore/Examples.lean` pins a counterexample to each: `RueCore.OwnSt.wf`, the state-against-type invariant (`.fields` at a scalar is a state no rule can write), and `RueCore.WfStructs`, §3's class assignment (a struct whose recorded class disagrees with its field join separates the two associations). `RueCore.OwnSt.setAt_wf` and `RueCore.Ctx.joinAll_wf` say the rules that write and that join keep the first, and `RueCore.Typed.wf` proves it preserved judgment-wide from a well-formed context; with `RueCore.fnCtx_wf` it holds at every normal outgoing state of a function body (`RueCore.Typed.wf_fnCtx`), with no hypothesis left over. It covers every delivered `break` state too, and `RueCore.Typed.skel_preserved` says each one extends the incoming skeleton (RUE-2369). That theorem was false while §5.7's `⊥` concluded at *any* context of the incoming skeleton (RUE-2340); since the judgment carries §5.3's `Ω` (RUE-2368), `⊥` has no state, and every normal outgoing state is one a rule wrote |
| §5.3's `Ω` and the `-Bottom` rules | **mechanized as written**, with the readings below. `RueCore.Typed` concludes at `RueCore.Out`, an optional normal state with the recorded deliveries; (Strict-Bottom) is one variant per strict context the fragment has (`binopBot`, `floatBinopBot`, `indexReadBot`, `indexWriteBotRhs`, `indexWriteBotIdx`, `assignBot`, `matchBot`, `iteBot`, `TypedArgs.consBot`), concluding at the construct's own type `T_E` with the premises that name it; (Seq-Bottom), (Let-Bottom) and (Return-Bottom) are `seqBot`, `letBot` and `retBot`; (Let) with a divergent tail is `letInDiv`; the branch join is over the arms that continue (`RueCore.Ctx.joinOpt`, `RueCore.Ctx.joinOpts`). (Sub-Never) is folded into the rules §5.7 types at `never`, because the fragment has no `never` type; the checker's `RueCore.CTy.never` is its image. Before RUE-2368 the judgment had no `Ω`: `return` and `@panic` concluded at any context, and the checker, which had to pick one, refused a diverging arm beside a continuing one in five seeded shapes the compiler accepts (`if_return_arm_affine`, `match_return_arm_linear`, `match_never_first_arm`, `if_panic_arm_linear`, `panic_past_linear`) |
| Dead code after a diverging form | **unchecked, as §5.3 writes it**. (Seq-Bottom), (Let-Bottom) and (Strict-Bottom) type nothing past a `return` or `@panic`, so the checker accepts ill-formed dead code the compiler rejects (E0206, E0205, E0406, E0478, E0203, E0600 there), which §5.3 permits a surface checker to do. The corpus verdict contract excludes such programs (`lean/RueCore/Corpus.lean`), and whether the core should say more about unreachable source is a question for the calculus. No seed case has the shape, and the generator draws `break` only as the last form of an arm or of a once-through loop body, with at most one diverging arm per branch, so no generated case has it either (RUE-2330); whether errors in unreachable source are normative is RUE-2376 |
| Reading: `return` and `@panic` checked where they fire | **a reading §5.7's closing note allows**. `RueCore.Out` records no `⟨ret, _⟩` or `⟨panic, _⟩` delivery. (Fn) §5.8 consumes a `⟨ret, Σ_e⟩` only for its residual-linear check, and `RueCore.Typed.ret` carries that check as a premise at the edge; §5.7 exempts `⟨panic, _⟩` from §5.6, so nothing consumes it. The recorded deliveries are the `⟨break, Σ⟩` ones (Loop-Break) §5.7 joins, threaded through every rule by §5.3's convention |
| Reading: the diverge edge checked frame-wide (RUE-2369) | **in effect**. (Fn) §5.8 checks a `⟨diverge, Σ_h⟩` delivery against the by-value parameters; `RueCore.Typed.loopDiv` and `RueCore.Typed.loopBreakDiv` check it where it fires, by `NoResidualLinear` at the loop-head state, against every binding in scope — the non-panic residual condition §5.6 and §5.7 retain for a `⊥_diverge` edge. It fires only when the body reaches its back edge, as §5.7 delivers `diverge` only then. The compiler agrees: a linear **local** live at `loop { }` is E0406, as a linear parameter is (probes recorded in RUE-2326's report) |
| Reading: `break` carries no value (RUE-2369) | **in effect, as the calculus writes it**. §2, §6.10 and `4.8:22` have no value-carrying `break` ("`break expr` is a compile-time error at the surface"), so `RueCore.Expr.brk` is the nullary `break` §2 writes, and a `break`-exited loop is `unit`-typed |
| §5.7's loop-head state and the back edge | **mechanized as an equation, with one added premise**. `RueCore.LoopHead` states `Σ_h = head(Σ, e)` over the body judgment at `Σ_h`: the join of the entry with the body's back-edge state, or the entry when the body never completes (the core has no `continue`, so the back-edge set has at most that one state). It admits any solution, as the calculus's `where` clause does; `RueCore.check` computes the least one by §5.7's iteration (`RueCore.headIter`, bounded by the body's size, so `check` stays total and a bound too small costs completeness, never soundness) and re-verifies the equation. The added premise is that a head a back edge produced is a state of its declared types (`Ctx.Wf`): §5's states are, `RueCore.Typed.wf` proves every state a rule writes is, but the equation alone has solutions that are not, because `Σ_h` is on both sides of it. The back-edge proof is `RueCore.LoopHead.reenter`: the join **absorbs** a second copy of its right arm (`RueCore.Ctx.join_absorb`, with idempotence `RueCore.OwnSt.join_idem`), so the loop's derivation at entry types it again at its head, and `soundness`'s fuel induction takes every later turn with the same body derivation; `RueCore.LoopHead.enter` and `RueCore.LoopHead.backEdge` carry the store's agreement onto the head from the entry and from the back edge |
| (Loop-Break)'s exits | **mechanized as written**. A `⟨break, Σ⟩` delivery (`RueCore.Typed.brk`) records the whole context at the `break`; the loop splits it at its own depth into the loop-local bindings still open there, which carry §5.6's obligation discharged at the exit (`RueCore.Ctx.loopLocals`, checked by `NoResidualLinear`), and `outside_loop(Σ_x)` (`RueCore.Ctx.outsideLoop`), which the loop joins over every exit (`3.8:80`). §6.10's unwind drops the loop-local cells newest-first (`RueCore.loop_exit_ok`). A body with no `break` targeting the loop delivers none (`RueCore.Typed.brk_nil`), which is how the syntactic premise of (Loop-Div) and the `Δ_out` of every loop agree. The corpus seeds RUE-1615's shape (a move in a loop whose every path breaks, accepted), RUE-1614's at the exit join (a linear value consumed on one exit only, E0443), a move meeting an outer loop's back edge, and a `break` past a live linear loop-local; the generator draws counted, once-through and nested loops with `break` arms that may move a binder from outside the loop (RUE-2330), and at `--gen 200 --seed 7` and `--gen 1000 --seed 23` every one of the 1,200 generated cases agrees with the compiler |
| Fuel monotonicity and no masking | `RueCore.fuel_mono` and `RueCore.no_masking` (`lean/RueCore/Soundness.lean`). A bound that produced a result other than `outOfFuel` produces that same result at every larger bound; a bound that reached a violation reaches that same violation at every bound that answers at all. Together they say the ∀-fuel form of the theorems above is a statement about one real outcome, and that no choice of fuel hides a violation behind exhaustion |

## ADR-0097's conditions, and the theorem that meets each

Decision 3 makes the `eval` form the primary statement and requires, before
checkpoint C, that it be tied to §6:

- *a mechanized §6 reduction relation*: `RueCore.Step`, the §6 row above;
- *`eval` sound with respect to it*: `RueCore.eval_sound`, resting on
  `RueCore.run_sim`, which needs no typing hypothesis;
- *`eval` complete modulo fuel*: `RueCore.eval_complete`, resting on
  `RueCore.eval_steps_of_outOfFuel`, and `RueCore.eval_diverges_iff` for the
  runs that do not end;
- *monotonicity and no-masking, so `outOfFuel` never stands in for a
  violation*: `RueCore.fuel_mono` and `RueCore.no_masking` from `eval`'s
  side, and from §6's side `RueCore.run_stuck_of_step_stuck` (a stuck §6 run
  is a refusal of `run` at every fuel past its length, on every program) and
  `RueCore.eval_diverges_iff` (on a checked program, exhaustion at every fuel
  is divergence).

Decision 5's CI gate has three conditions. **(b)**, "that theorem is about §6
and not only about `eval`: the fuel lemmas and the adequacy lemma are proved
for the same fragment", is met by the four items above over the fragment
this document states, and its consequence in §7's terms is
`RueCore.step_progress`, `RueCore.step_preservation` and
`RueCore.step_type_safety`. In particular "for every fuel, never `.stuck`"
implies that no reduction sequence reaches a stuck configuration on every
program (`RueCore.step_never_stuck_of_run`), and on a checked program the two
are equivalent (`RueCore.never_stuck_iff`). **(a)**, the safety theorem
covering every Phase C slice, still waits on drop order (RUE-2237); **(c)**,
the independent review, is checkpoint C (RUE-2251).

## Traceability

`lean/INDEX.md` is the generated index (`scripts/validate-lean-xref-index.py`,
held fresh by the premerge tier): every declaration with the calculus rules,
sections, and prose paragraphs its doc-comment cites, and every labeled rule
and section of §5 and §6 with the declaration that mechanizes it or *not yet
mechanized*. The theorem side of the index, one row per §7 bullet naming its
theorem, is this document; RUE-207 completes it.
