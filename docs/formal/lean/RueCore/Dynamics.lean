import RueCore.Statics

/-!
# RueCore.Dynamics — the executable machine (§6)

A definitional interpreter over the §6.1 configuration shape, restricted to
the fragment: a store of single-cell binding allocations (`full c`, whose
contents is a tree with §6.1's moved-out marker `⊘` allowed at any node, or
the retired marker `†` = `dead`), a frame holding the environment `ρ` and the
scope record `σ`, and a drop trace — the fragment's image of the oracle's
observable `Outcome` (drop trace + result).

Design commitments carried over from §6:

* **Memory violations are refusals, not silence.** Reading a `⊘`/`†` cell,
  implicitly dropping a linear value, or overwriting one, yields a named
  `Violation`. The §7 safety theorem (`Soundness.lean`) is exactly:
  well-typed programs never reach one — which is a claim about the refusals
  below, and the carve-out under "Pending values" names the one edge they
  do not cover. The refusals are of two kinds, and the distinction matters
  for what `eval` is a model of:
  - `useAfterMove`, `useAfterDrop`, `unbound`, and `typeConfusion` are
    §6's own stuck states: no reduction rule applies to a read of a `⊘` or
    `†` cell, an unbound index, or an operator on a value of the wrong
    shape.
  - `linearLeak`, `linearOverwrite`, and `linearDiscard` are **monitors**
    the interpreter adds. §6.7's `endscope`, §6.8's overwrite-drop, and
    §6.7's temporary discard execute the drop and rely on §5 (`3.8:32`,
    `3.8:77`, `3.8:64`) to have excluded the linear case; on a program §5
    rejects, the paper relation drops the value and this machine refuses.
    The monitors make a linear violation observable as a positive result
    (which is what `soundness` needs), at the price that `eval` and §6
    differ on statically invalid input.
  - `ownedUnderCopy` is a fourth monitor of the same kind (RUE-2323): the
    **copy-closure** check (`Contents.copyClosed`) at aggregate introduction
    and at an assignment. §6.5's (D-Struct) builds whatever its initializers
    produced and relies on (Struct-Intro) §5.8 to have made a `Copy`
    struct's fields `Copy`; on a program §5 rejects, an owned value can sit
    under a `Copy` node, and the next (D-Use-Copy) duplicates its owner. The
    monitor refuses that aggregate before it exists, which is what lets
    `no_double_free` (`Trace.lean`) be proved without a typing derivation.
* **The correspondence with §6 is claimed on the checker's input domain.**
  On a program `check` accepts, `eval` and §6 agree: `eval_sound`
  (`Adequacy.lean`) proves `eval ⇒ Step*` there, and `eval_complete` the
  converse modulo fuel; on other input they may not. An
  operator reduces both its operands first, in §6.2's own left-to-right
  order, and only then inspects their shapes, so a mismatch is named after
  the operands have run; but a raw `intLit` outside its type's `n_T` range
  evaluates to its value here, while §6's integer domain is bounded (§6.1)
  and `check` rejects the literal (`Examples.lean` pins both).
* Scope exit *retires* the binding's allocation (`drop-retire`, §6.1), so a
  use after scope exit is `useAfterDrop`, distinct from `useAfterMove`.
* A read that reaches a `⊘` — at the place itself or anywhere inside the
  aggregate it names — is `useAfterMove`. The second case is the one
  `fully-owned(Σ, p)` (§5.1) excludes: handing an aggregate with a hole in it
  to a new owner (`3.8:26`).
* `@drop` and overwrite-drop do **not** retire (§6.8/§6.11): the binding
  stays reinitializable.
* Arithmetic overflow, a zero divisor, an out-of-range or negative array
  index, an out-of-range `@intCast` and an
  explicit `@panic` are *panics* (`↯` in §6.12), a defined outcome permitted
  by the safety theorem, not a violation. A panic carries the trace the run
  had already produced, because §6.12's observable output survives the trap
  (the process prints what it printed and then exits 101), and because
  §5.7's `⊥_panic` runs no further drop there is nothing to add to it.
* Traces record each drop (`drop ℓ v`) and each discarded temporary
  (`dropTemp v`) — the §6.7 temporary-death analog — and, nested under
  either, every user destructor §6.11 runs (`dtor s v`). Beside them
  `@dbg`'s own event (`dbg v`) is the other half of §6.12's observable
  output; those two are what a printed Rue program can see, in the one
  trace order they happened in.
* Every aggregate value carries a **value identity**, minted at its
  introduction ("Value identities" below), so each drop event says *which*
  value it dropped, not only what it looked like.

## Value identities (RUE-2323)

§6.1 gives allocations identities and values none: two `S1 { 1 }` values are
the same term. To state §7's no-double-free bullet over the trace — "every
stored value's destructor runs at most once" — the machine needs to tell two
equal-looking values apart, so aggregate introduction ((D-Struct) and
(D-Array) §6.5, (D-Enum-Intro) §6.6, and the repeat form) **mints** one
(`introVal`). The identity is the store's next index, reserved by appending
`†`: §6.1 already says "a fresh identity is one not in `dom(H)`" and that a
dead index is never reused, so a reserved index is a name nothing else can
take, and threading a counter through `eval` would add nothing the store
does not already count. The slot holds no storage, and nothing binds it or
reads it.

The identity rides on the value (`Val.struct s i vs` and the other aggregate
forms) and on its stored image (`Contents`), through every move, parameter
and `match` binding, and the events carry it because they carry the value or
contents they ran on. It is unobservable: nothing in `eval` branches on it,
and the printer (`Print.lean`) and the corpus (`Corpus.lean`) never print it,
so the bridge compares exactly what it compared before. A copy of a `Copy`
value carries its original's identity; `no_double_free` counts only the
non-`Copy` nodes (`Contents.own`, `Trace.lean`), which are never copied.
`Step` mints the same way, so the two presentations keep one store
(`Step.lean`).

## Places, partial moves, and §6.11's drop order

A place is a root binding and a path of field and constant-index steps
(`Place`, `Syntax.lean`),
and §6.3 navigates it into the stored contents: `H(ℓ)@π` is `readAt` and
`H[ℓ@π ↦ ⊘]` is `writeAt`. (D-Use-Move) writes `⊘` at exactly the
sub-position moved — the whole cell for a whole-place use, one field for a
projection, which is the **partial move** of §4.2 — so the later scope-exit
drop of `ℓ` skips it and cannot free it a second time. That skip is
`dropContents`'s `⊘` case, and it is what makes the residual drop of `3.8:60`
path-specific (`3.8:73` is the array-element form of the same rule).

A struct's contents is its declaration's index and one contents per field, so
the drop walk is §6.11's own: the user destructor first (`3.9:28`), then the
fields in declaration order (`3.9:13`), recursively, skipping every `⊘`. Every
path that drops — `@drop` (§6.11), scope exit (§6.7), the overwrite (§6.8), a
discarded temporary (§6.7), and the frame teardown (§6.9) — routes through it,
so a trace carries that order wherever a drop happens.

An **array**'s contents is its element type and one contents per element, and
§6.11 drops those in **ascending index order** (`3.9:15`, `3.8:73`) with no
destructor of its own to run first — `3.9:14` gives `[T; n]` a destructor
exactly when `T` has one. A constant index is a step of `π` like a field slot
(`Place.idx`, `Syntax.lean`), so `readAt`/`writeAt` navigate it with the same
two lines; what a *dynamic* index needs instead is a bounds check, below.

Both linear monitors read the **residue** rather than a type. §6.7's and
§6.9's leak monitor refuses when the contents a scope exit reaches still holds
a live declared-`linear` sub-value (`Contents.residualLinear`, §5.6's own
recursion), and §6.8's overwrite monitor refuses on the same reading of the
position being written. A carrier whose linear field has been moved out
therefore drops its remaining residue quietly, which is the RUE-1591 model and
what the compiler does.

## The declared-linear destructure (§6.3)

(D-Use-Declared-Linear) §6.3 is a **distinct redex**, and `eval`'s `use`/`drop`
cases branch on it before anything else: where the path has a proper prefix `d`
of declared-`linear` struct type, `split(H(ℓ)@π_d, π_s)` exposes the selected
leaf and the ordered residue, `drop*` destroys the residue left to right, and
only then does `ℓ@π_d` — the *consumed place*, not the leaf's position — become
`⊘`. `Contents.splitResidue`/`Contents.splitFields` are `split`, walking fields
in declaration order, recursing at the selected step and appending the whole
subtree at every unselected one, so nested residue is visited before a later
sibling (probe d14). `dropResidue` is the `drop*`, and `Contents.destructure`
is §6.3's composition of the two.

**Where the plan comes from.** §6.3 says the machine consumes a closed
elaboration annotation `μ` and "never consults `Γ`, recompute[s] a
declared-linear prefix, or recover[s] constant-index provenance after
reduction". This fragment's `Expr.use`/`Expr.drop` carry no annotation — they
carry a `Place`, and the machine has no type environment — so `eval` recovers
the plan from the **store** instead (`Contents.declaredPlan`), which it can do
because §6.1's struct value names its declaration. That is a deviation in
*form* only: `ContentsMatches.declaredPlan_eq` (`Soundness.lean`) proves the
store-side plan is the type-side `declaredPrefix` (`Syntax.lean`) at every
place a matched cell answers for, so the redex that fires is the one
elaboration would have annotated. Every index a `Place` carries here is a
**constant** step (`Place.idx`), so the stored aggregate answers for the whole
path. A selected path may pass through an index step
(`Examples.destructureThroughIndex`), and `declaredPlan_eq` still needs no `μ`
on the syntax: the plan is decided by the types along the path, index steps
included.

**The residue monitor.** §6.3 excludes a linear residue by the
(Use-Declared-Linear-Destructure) premise "before this redex can fire", so the
paper machine has nothing to check. `dropResidue` checks anyway, refusing with
`linearLeak` where a retained subtree still holds a live declared-`linear`
value: the same monitors-not-silence commitment §6.7's `endscope` and §6.8's
overwrite already make. `ContentsMatches.destructure_ok` (`Soundness.lean`) is
the proof that a program `check` accepts never reaches it. `@drop` at a
declared plan makes the same commitment at the selected **leaf**: a `⊘` there
refuses with `useAfterMove` rather than dropping nothing, because
`Contents.mult ⊘ = .copy` would otherwise make the redex succeed in silence
(`Examples.dropDeclaredHoleLeaf`).

## The bounds trap (§6.5, §6.12)

§6.5 bounds-checks an array index "at the moment the path is navigated"
(`7.1:10`), before the element is read: in range, (D-Index) yields `vi` and
§6.3 copies it; out of range or negative, (D-Index-Trap) abandons the
configuration to `↯bounds`, §6.12's own category, which the implementations
report as `index out of bounds` and which exits 101 like every other trap. It
is a **defined** outcome, not a violation: the safety theorem permits it
exactly as it permits an overflow. Only the dynamic-index forms
(`Expr.indexRead`, `Expr.indexWrite`, `Expr.indexDrop`) can reach it — a constant index is checked by `Ty.atPath` at compile time
(`7.1:9`), so `Place.idx` never traps.

## Pending values: the one edge no monitor covers

The general shape is a value already built for a **sibling position** that a
later sibling destroys by `return` or by `break`. Every list of subexpressions `evalArgs`
walks has it: a call's argument list, a struct literal's initializers, and an
array literal's elements. So does a fourth position, an assignment's
right-hand side while the target's indices run after it (`5.2:14`, §6.2's
`assign p[ v̄, E, … ] = v`), which `indexWrite` threads through `andThen`
rather than `evalArgs`. Such a value lives in no cell and in no scope
record — between the `use` that produced it and the `mintParams` that gives a
by-value argument one (§6.9's (D-Call)), between an initializer and the
`mkStruct`/`mkArray` that would have aggregated it, or between a right-hand
side and the store that would have written it. If a later sibling unwinds
by `return`, (D-Return) §6.9 discards the evaluation context — `g(v̄, …, E, …)`,
the aggregate contexts `S { v̄, …, E, … }` and `[ v̄, …, E, … ]`, and
`assign p[ v̄, E, … ] = v` with it — and runs `run-all-scope-drops` on the frame's records, which never named that
value; if it unwinds by `break`, (D-Break) §6.10 discards the same context
(`E'`) and the loop's `unwind-drops` walks the same records. Its drop is therefore neither run nor monitored, whatever its
multiplicity class: an affine sibling emits no `dropTemp`, and a linear one is
destroyed without a `linearDiscard`. At the right-hand side only the affine
half applies: (Assign)'s leaf premise `class(T) ≠ Linear` (`3.8:77`) keeps
the abandoned value from being linear, so `no_linear_discard` is not affected
by that position.

That is the calculus as written, not a modelling slip. (D-Return) and
(D-Break) unwind σ and nothing else, and §5's only bottom rule for an argument position — §5.3's
strict-context bottom rule, `Strict-Bottom` there, which `Typed.consBot` and
the other `-Bottom` variants mechanize — carries `⊥;Δ_e` outward without imposing §5.3's discard check on
the siblings already evaluated, so the statics accept the program exactly as
the calculus does. §6.9's own justification for
(D-Return) — "every bound cell is also registered in the frame's scope
records" — is exactly true and exactly insufficient here, because a sibling
temporary is not a bound cell. The Rue compiler behaves the same way (a
destructor-bearing sibling's destructor does not run, at an argument, a
struct initializer, an array element and an assignment's right-hand side
alike — probe r14 of RUE-2342), so the bridge cannot see it either.

`eval` models the calculus rather than patching it, so no monitor is added:
`evalArgs` passes a `returned` or `broke` abort on untouched — and because the three list
forms share that one function, all three share the edge; `indexWrite`'s
`andThen` passes it on the same way at the right-hand side. The shapes are
pinned as kernel-checked witnesses in `Examples.lean`
(`linearLostAtCallArg`, `affineLostAtCallArg`, `linearLostAtArrayElem`, and
`linearLostAtBreakArg` for the `break` case, which the compiler matches), the
§7 claim is stated with
the carve-out named (`Soundness.lean`'s `no_violation`,
`docs/formal/03-metatheory.md`), and closing it is an open spec decision
(RUE-2316, the pending-argument decision) — it needs a rule, in §5.7, §6.9
or §6.10, before a monitor here would mean anything.

## Frames, scope records, and unwinding (§6.1, §6.9)

§6.1's frame is `φ = ⟨ρ ; σ⟩` with `σ` a *stack* of open scope records.
(D-Let) §6.7 and (D-Match) §6.6 both **append** their cells to the innermost
record rather than pushing a new one, so `Frame` carries that one record.
§6.10's loop does push a scope, `push-scope(φ)`, and `unwind-drops(H, φ', φ)`
runs the drops of every scope open in `φ'` that is not open in `φ`. The
fragment keeps one record and reads the loop's boundary as its **length**
at the loop's entry: a loop body appends to the record like any other form, a
`break` hands the loop the record of the frame it fired in
(`EvalRes.broke`), and the cells past the loop's length are exactly the
scopes §6.10 would pop — which the loop drop-retires newest-first. A body that
completes has closed its own scopes on the way (§6.7's `endscope`), so
(D-Loop-Iter)'s `run-scope-drops` has nothing left to run.

A `match` arm's payload cells are registered exactly as a `let`'s cell is, in
both books: appended to the record *and* owed to the arm's `endscope` marker
(§6.6). So the arm's normal path drops them when its body becomes a value —
`6.3:17`'s timing, newest-first — and an early `return` finds them in σ
instead, which is the same RUE-1277 redundancy read at a second binder form.

The record is the RUE-1277 redundancy, and it is load-bearing here: a `let`
registers its cell **both** in the scope record and in the administrative
`endscope` form that the normal path runs (modelled by the structure of
`eval`'s `letIn` case). An early `return` throws the `endscope` markers away
with the evaluation context, and `run-all-scope-drops` walks the record
instead, so every live binding of the frame is still dropped (`3.9:18`, which
lists a `return` among the points drops are inserted at), newest-first
(`3.9:4`, reverse declaration order) — §6.9's (D-Return).

## Fuel (ADR-0097, RUE-2233)

Calls make the machine's recursion unbounded — a recursive callee's body is
not a subexpression of the call — so `eval` is indexed by a fuel that every
step spends, and returns `outOfFuel` when it runs out. Counting *steps*
rather than only calls is what keeps the interpreter structurally recursive
on the fuel alone: totality no longer depends on the expression's shape,
which is precisely what recursion breaks. Unspent fuel costs nothing to
evaluate, so a large bound is free.

Two lemmas (`Soundness.lean`) say the resulting ∀-fuel theorem is not
vacuous: `fuel_mono` (a result other than `outOfFuel` is the result at every
larger fuel) and `no_masking` (a refusal at one fuel is that same refusal, or
`outOfFuel`, at every other — no bound turns a violation into exhaustion).
-/

namespace RueCore

/-- Machine values (§6.1's `v`), fragment forms only. `struct s i vs` is §6.1's
`{ v1, …, vk }_S`: the declaration's index, the value's **identity** `i`
(minted at introduction, `introVal`; §6.1 has none, and the module docstring
says why the machine adds it), and one value per field, in
declaration order — the order `3.9:13` drops them in. `float w f` is §6.1's `f_T` at `T = float(w)`: §2's
datum, not a bit pattern (`Float.lean`). A struct value names its declaration rather than
carrying its class, so the machine's drop decisions are value-driven — it
reads the tag the value carries — while the class and the destructor come from
the program's declarations, as the compiled program's drop glue does.

`enum e k i vs` is §6.1's `Kj⟨ v1, …, va ⟩`: the declaration's index, the
**0-based variant tag** `Kj`, the identity, and the active variant's payload;
`array T i vs` is `[ v1, …, vn ]` with its element type and identity. `vs = []` is
§6.1's bare tag, the discriminant-only case, which `6.3:15` lets an
implementation store as a plain discriminant. The tag is what §6.6's (D-Match)
switches on and what §6.11's enum case reads to find the one payload to drop. -/
inductive Val where
  | int (w : IntWidth) (s : Sign) (n : Int)
  | float (w : FloatWidth) (f : FloatDatum)
  | bool (b : Bool)
  | unit
  | struct (s : Nat) (id : Nat) (fields : List Val)
  | enum (e : Nat) (k : Nat) (id : Nat) (payload : List Val)
  | array (elem : Ty) (id : Nat) (vs : List Val)
deriving Repr

/-- The dynamic image of `class(T)` (§3) on a value: scalars are `Copy`, a
struct value has the class its declaration records. -/
def Val.mult (D : Decls) : Val → Mult
  | .struct s _ _ => D.classOf s
  | .enum e _ _ _ => D.enumClassOf e
  | .array T _ vs => Ty.mult D (.array T vs.length)
  | .int _ _ _ | .float _ _ | .bool _ | .unit => .copy

/-- Cell contents (§6.1's `c ::= v | ⊘`), as a **tree**: `⊘` may sit at any
node, not only at the root, because (D-Use-Move) §6.3 writes `H[ℓ@π ↦ ⊘]` at
exactly the sub-position a partial move takes (§4.2, `3.8:22`). A hole-free
contents is a value (`toVal`), and a value written into a cell becomes the
hole-free tree of the same shape (`ofVal`); the two are inverse, which is what
lets §6.11's walk and §6.3's navigation share one representation. An aggregate
node keeps its value's identity, so a stored value and the value read back
out of it are the same value. -/
inductive Contents where
  | hole
  | int (w : IntWidth) (s : Sign) (n : Int)
  | float (w : FloatWidth) (f : FloatDatum)
  | bool (b : Bool)
  | unit
  | struct (s : Nat) (id : Nat) (cs : List Contents)
  | enum (e : Nat) (k : Nat) (id : Nat) (cs : List Contents)
  | array (elem : Ty) (id : Nat) (cs : List Contents)
deriving Repr

mutual
/-- A value stored into a cell or a sub-position: the same tree with no `⊘` in
it (§6.8's `H[ℓ@π ↦ v]`, §6.7's (D-Let)) (helper). -/
def Contents.ofVal : Val → Contents
  | .int w s n => .int w s n
  | .float w f => .float w f
  | .bool b => .bool b
  | .unit => .unit
  | .struct s i vs => .struct s i (Contents.ofVals vs)
  | .enum e k i vs => .enum e k i (Contents.ofVals vs)
  | .array T i vs => .array T i (Contents.ofVals vs)

/-- `ofVal` over a field or element list (helper). -/
def Contents.ofVals : List Val → List Contents
  | [] => []
  | v :: vs => Contents.ofVal v :: Contents.ofVals vs
end

mutual
/-- The value a contents denotes, or `none` when a `⊘` sits somewhere in it —
which is the read §6.3 leaves stuck and §7's no-use-after-move bullet forbids
(helper). -/
def Contents.toVal : Contents → Option Val
  | .hole => none
  | .int w s n => some (.int w s n)
  | .float w f => some (.float w f)
  | .bool b => some (.bool b)
  | .unit => some .unit
  | .struct s i cs => (Contents.toVals cs).map (Val.struct s i)
  | .enum e k i cs => (Contents.toVals cs).map (Val.enum e k i)
  | .array T i cs => (Contents.toVals cs).map (Val.array T i)

/-- `toVal` over a field or element list (helper). -/
def Contents.toVals : List Contents → Option (List Val)
  | [] => some []
  | c :: cs =>
      match Contents.toVal c, Contents.toVals cs with
      | some v, some vs => some (v :: vs)
      | _, _ => none
end

/-- Whether a position holds §6.1's `⊘` — the test `@drop` makes before it
runs a drop, so that dropping an already moved-out place is the refusal §7's
no-use-after-move bullet forbids rather than a silent no-op (helper). -/
def Contents.isHole : Contents → Bool
  | .hole => true
  | .int _ _ _ | .float _ _ | .bool _ | .unit | .struct _ _ _ | .enum _ _ _ _
  | .array _ _ _ => false

/-- The dynamic image of `class(T)` (§3) on cell contents: a hole has nothing
to drop, and a struct has the class its declaration records (helper). -/
def Contents.mult (D : Decls) : Contents → Mult
  | .struct s _ _ => D.classOf s
  | .enum e _ _ _ => D.enumClassOf e
  | .array T _ cs => Ty.mult D (.array T cs.length)
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => .copy

mutual
/-- Whether every node of a contents is `Copy` — a `⊘` and a scalar are, and an
aggregate is when its own class is and each member is (helper). -/
def Contents.allCopy (D : Decls) : Contents → Bool
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => true
  | .struct s _ cs => decide (D.classOf s = .copy) && Contents.allCopyList D cs
  | .enum e _ _ cs => decide (D.enumClassOf e = .copy) && Contents.allCopyList D cs
  | .array T _ cs =>
      decide (Ty.mult D (.array T cs.length) = .copy) && Contents.allCopyList D cs

/-- `allCopy` over a field, payload or element list (helper). -/
def Contents.allCopyList (D : Decls) : List Contents → Bool
  | [] => true
  | c :: cs => Contents.allCopy D c && Contents.allCopyList D cs
end

mutual
/-- **Copy closure**: no non-`Copy` value sits under a `Copy` node. §3 makes
it a fact about types — a `Copy` type's fields, payloads and elements are
`Copy` (`3.8:18`, `6.3:19`, §3's array lift) — and the machine relies on it
wherever it duplicates a value: (D-Use-Copy), the dynamic-index read and the
repeat form copy a `Copy` value whole, which is sound only when nothing owned
hides inside it. A struct literal, an enum literal or an array literal whose
class is `Copy` but whose members are not, or an assignment that writes an
owned value under a `Copy` node, is a shape no well-typed program produces;
the machine refuses it (`ownedUnderCopy`) rather than build a duplicable
owner, and `soundness` proves a checked program never reaches the refusal
(`HasTy.copyClosed`, `ContentsTy.copyClosed`, `Soundness.lean`). It is a
**monitor** in the module docstring's sense — §6's (D-Struct) would build the
value — and it is what makes `no_double_free`'s conservation law hold without
a typing derivation (`Trace.lean`) (helper). -/
def Contents.copyClosed (D : Decls) : Contents → Bool
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => true
  | .struct s _ cs =>
      if D.classOf s = .copy then Contents.allCopyList D cs else Contents.copyClosedList D cs
  | .enum e _ _ cs =>
      if D.enumClassOf e = .copy then Contents.allCopyList D cs
      else Contents.copyClosedList D cs
  | .array T _ cs =>
      if Ty.mult D (.array T cs.length) = .copy then Contents.allCopyList D cs
      else Contents.copyClosedList D cs

/-- `copyClosed` over a field, payload or element list (helper). -/
def Contents.copyClosedList (D : Decls) : List Contents → Bool
  | [] => true
  | c :: cs => Contents.copyClosed D c && Contents.copyClosedList D cs
end

/-- `toVals` does not change a list's length, for `mult_toVal`'s array case
(helper). -/
theorem Contents.toVals_length : ∀ (cs : List Contents) (vs : List Val),
    Contents.toVals cs = some vs → vs.length = cs.length
  | [], vs, h => by simp [Contents.toVals] at h; subst h; rfl
  | c :: cs, vs, h => by
      simp only [Contents.toVals] at h
      split at h
      · rename_i v vs' hv hvs
        cases h
        simp [Contents.toVals_length cs vs' hvs]
      · cases h

/-- `Contents.mult` agrees with `Val.mult` on a hole-free contents: §6's
`Step.indexDrop` reads `leaf.mult` on the store's `Contents`, while `eval`'s
dynamic checks (RUE-2400) read `v.mult` on the `Val` a successful read
produces; this is what lets the two land on the same refusal. Serves
RUE-2289 part 2, the `eval ⇒ Step*` simulation. -/
theorem Contents.mult_toVal (D : Decls) (c : Contents) (v : Val) (h : c.toVal = some v) :
    c.mult D = v.mult D := by
  cases c <;> simp [Contents.toVal] at h
  all_goals (first | (subst h; rfl) | skip)
  all_goals (obtain ⟨vs, hvs, rfl⟩ := h)
  all_goals (simp [Contents.mult, Val.mult, Contents.toVals_length _ _ hvs])

mutual
/-- §5.6's `residual-linear`, read on the **contents** rather than on Σ: does a
live sub-value of a declared-`linear` struct type remain? This is the leak
monitor §6.7's `endscope` and §6.9's frame teardown consult, and the overwrite
monitor of §6.8. A `⊘` carries nothing (`3.8:60`'s skip), a live
declared-`linear` struct carries the obligation itself (`3.8:74`), and
otherwise the obligation is the disjunction over the live fields — exactly the
recursion §5.6 writes for Σ, on the store's side of the invariant.

At an **enum** the residue is the **active** variant's payload and nothing else:
an enum declares no attribute to carry an obligation of its own, and the
inactive variants have no storage (§6.11). That is weaker than §5.6's Σ-side
clause, which reads `class(E) = Linear` over *every* variant because the tag is
not a static fact — and weaker in the safe direction: a program the statics
accept has no linear payload in any variant, so the monitor finds none under the
tag either (`ContentsTy.residualLinear_false`, `Soundness.lean`). The gap is
exactly probe e11, which the statics reject (E0406) and which this monitor would
let run. -/
def Contents.residualLinear (D : Decls) : Contents → Bool
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => false
  | .struct s _ cs =>
      (match D.structs[s]? with
       | some sd => sd.attr = .linear || Contents.residualLinearList D cs
       | none => false)
  | .enum _ _ _ cs => Contents.residualLinearList D cs
  -- An array declares no attribute of its own, so its obligation is the
  -- disjunction over its live elements — and a zero-length one carries
  -- nothing at all, which is `3.8:74` read on the store's side.
  | .array _ _ cs => Contents.residualLinearList D cs

/-- The same over a field, payload or element list (helper). -/
def Contents.residualLinearList (D : Decls) : List Contents → Bool
  | [] => false
  | c :: cs => Contents.residualLinear D c || Contents.residualLinearList D cs
end

/-- `H[ℓ@π ↦ c']` (§6.3's (D-Use-Move), §6.8's `place_write`): replace the
sub-position at a path. `none` is a step that is not a field of what is
stored, which is the same navigation `readAt` refuses with `typeConfusion` —
so every `eval` arm that writes has already read at the same path, and its
`none` case is unreachable rather than a second refusal (helper). -/
def Contents.writeAt : Contents → List Nat → Contents → Option Contents
  | _, [], new => some new
  | .struct s i cs, f :: π, new =>
      (match cs[f]? with
       | some c => (Contents.writeAt c π new).map fun c' => .struct s i (cs.set f c')
       | none => none)
  | .array T i cs, f :: π, new =>
      (match cs[f]? with
       | some c => (Contents.writeAt c π new).map fun c' => .array T i (cs.set f c')
       | none => none)
  | _, _ :: _, _ => none
  -- An enum node has no field step: a payload is reached by a `match` arm's
  -- binding, never by a path (§5.6, `Syntax.lean`), so this falls to the
  -- catch-all above with every other non-struct node.

/-- The store `H` (§6.1) holds one cell per binding allocation; the cell is
live contents or the retired marker `†`. §6.1's whole-cell `⊘` is
`full .hole` — a hole at the root of the tree, which is what a whole-place
move writes. -/
inductive Cell where
  | full (c : Contents)
  | dead
deriving Repr

/-- The store (§6.1): locations are indices; allocation appends. A dead
cell keeps its index occupied — identities are never reused (§6.1). -/
abbrev Store := List Cell

/-- The environment `ρ` (§6.1), de Bruijn: index `i` ↦ its location. -/
abbrev Env := List Nat

/-- §6.1's frame `φ = ⟨ρ ; σ⟩`: the environment and the frame's open scope
record — the cells owed a drop when the frame's scopes end, in creation order
(dropped newest-first). Every binding of the fragment is a `let` binding or a
by-value parameter, and both are registered; `borrow`/`inout` parameters,
which are deliberately never registered (§6.9, `3.8:62`), are not in the
fragment. -/
structure Frame where
  env : Env
  scope : List Nat
deriving DecidableEq, Repr

/-- Drop events, the fragment's slice of the oracle `Outcome`. `drop ℓ c` and
`dropTemp v` mark *where* a drop starts — a binding's drop (§6.11: at scope
exit §6.7, at an unwinding exit §6.9, at `@drop`, or at an overwrite §6.8) and
a discarded temporary's drop ((D-Seq), §6.7) — and `dtor s c` is the one event
a Rue program can *observe*: the user destructor `S` declares (`3.9`), which
§6.11 runs before the value's fields. The events a drop emits follow each other
in §6.11's order: the destructor, then the fields in declaration order, each
recursively, with every `⊘` skipped. A binding's drop carries the *contents*
it ran on, because after a partial move what is dropped is a tree with holes in
it rather than a value; a discarded temporary is always a whole value. Every
aggregate in what an event carries has its identity, so the trace records
*which* values were dropped and destroyed; `no_double_free` (`Trace.lean`) is
the theorem that no identity is freed, or destroyed, twice. -/
inductive Event where
  | drop (ℓ : Nat) (c : Contents)
  | dropTemp (v : Val)
  | dtor (s : Nat) (c : Contents)
  /-- **A consumption** (RUE-2427): the aggregate nodes of `c` end here without
  a drop of their own, because every member they held has already been moved
  out or dropped — a `match`'s scrutinee shell once (D-Match) §6.6 has bound its
  payload to the arm's cells, and the path from a declared-`linear` place `d`
  down to the selected leaf once §6.3's destructure has handed the leaf on and
  dropped the residue. `c` is the shell itself, every member a `⊘`
  (`Contents.enumShell`, `Contents.skeleton`). No destructor runs: an enum
  declares none (§3, E0417), and `3.9:34` keeps a destructor-bearing value off
  a destructure's path. Like `drop` and `dropTemp` it is a marker no Rue
  program can observe (`Corpus.eventLine`); it is what lets
  `drop_exactly_once` (`TraceExact.lean`) name the end of *every* owned value
  in the trace. -/
  | consume (c : Contents)
  | dbg (v : Val)
deriving Repr

/-- Defined traps (§6.12's `↯κ`), the categories the fragment reaches.
`bounds` is the array index's — a negative or out-of-range one (`7.1:11`,
`4.11:9`), reported as `index out of bounds` and, like every trap, exiting
101; `rem-zero` and `user` are §6.12's
own spellings, and `cast-overflow` is the one §6.12 gains with `@intCast`
(`4.13:28`) — the implementations report it as `integer cast overflow`,
distinct from the arithmetic overflow, so the model keeps them apart. -/
inductive PanicKind where
  | overflow
  | divZero
  | remZero
  | castOverflow
  | bounds
  | user
deriving DecidableEq, Repr

/-- The named memory violations: the machine's refusals. §7's decomposed
memory-safety bullets each forbid one of these. -/
inductive Violation where
  /-- Reading a `⊘` cell (§7: no use-after-move). -/
  | useAfterMove
  /-- Touching a `†` cell (§7: no use-after-drop). -/
  | useAfterDrop
  /-- A scope exit — at a `let`'s end (§6.7) or on a frame's unwind (§6.9) —
  reaching a live linear value (§7: consumed exactly once; §5.6); or a
  declared-linear destructure whose residue holds one, which §5.1's
  `¬ linear-residue(S, π_s)` premise forbids (`3.8:60`, E0474) and which §6.3
  therefore leaves unchecked. -/
  | linearLeak
  /-- Overwrite-drop of a live linear value (§5.2, `3.8:77`). -/
  | linearOverwrite
  /-- Sequence-discard of a linear value (§5.3, `3.8:64`). -/
  | linearDiscard
  /-- A dangling index (impossible for elaborated programs; §2). -/
  | unbound
  /-- An operator on a wrong-shaped value, or a call whose argument count
  does not match the callee's parameter list (impossible for well-typed
  programs; §5.8, `4.10:3`). -/
  | typeConfusion
  /-- An owned value under a `Copy` node (§3: a `Copy` type's fields, payloads
  and elements are `Copy`, `3.8:18`, `6.3:19`) — the shape a copy would
  duplicate an owner through, which §7's no-double-free bullet forbids. The
  copy-closure monitor (`Contents.copyClosed`) refuses it where it could be
  built: at aggregate introduction and at an assignment (RUE-2323). -/
  | ownedUnderCopy
deriving DecidableEq, Repr

/-- `H(ℓ)@π` (§6.3): follow a path into the stored contents. Reaching a `⊘`
with path left to walk is the use of a moved-out place (§7's first bullet); a
step that is not a field of what is stored is a shape no well-typed program
produces (helper). -/
def Contents.readAt : Contents → List Nat → Except Violation Contents
  | c, [] => .ok c
  | .hole, _ :: _ => .error .useAfterMove
  | .struct _ _ cs, f :: π =>
      (match cs[f]? with
       | some c => Contents.readAt c π
       | none => .error .typeConfusion)
  -- A **constant** index step (`Place.idx`): the element is at `cs[c]`, and
  -- `Ty.atPath` has already checked `c < n` (`7.1:9`'s compile-time bounds
  -- check), so a `none` here is the same unreachable shape the struct arm's
  -- is. A *dynamic* index is not a path: `Contents.resolveDyn` bounds-checks
  -- it (§6.5's (D-Index-Trap)) and only then hands this function the constant
  -- path it resolved to.
  | .array _ _ cs, f :: π =>
      (match cs[f]? with
       | some c => Contents.readAt c π
       | none => .error .typeConfusion)
  | _, _ :: _ => .error .typeConfusion

/-- §6.5's bounds check on a **dynamic** index: `0 ≤ i < n`, the premise that
separates (D-Index) from (D-Index-Trap). A negative index is out of range
exactly as an oversized one is (`7.1:11`, `4.11:9`), which is why the test is
stated over `Int` rather than over `Nat`, and it is checked "at the moment the
path is navigated" (`7.1:10`). It is a named function rather than an inline
condition because the machine and its instrumented mirror (`Explain.lean`)
must test the same thing, and because §7's proof reads it twice. -/
def inBoundsIdx (i : Int) (n : Nat) : Bool := decide (0 ≤ i) && decide (i < (n : Int))

/-- The bounds test, read as §6.5 states it (helper). -/
theorem inBoundsIdx_eq_true {i : Int} {n : Nat} :
    inBoundsIdx i n = true ↔ (0 ≤ i ∧ i < (n : Int)) := by
  simp [inBoundsIdx]

/-- Where the dynamic tail of a place lands: the constant path it resolves to
once every index is a value and in range, §6.5's bounds trap, or a refusal
(helper). -/
inductive DynStep where
  | ok (ρ : List Nat)
  | bounds
  | stuck (w : Violation)

/-- **Resolve a dynamic tail** `[i₁]π₁…[iₖ]πₖ` against the contents it is taken
in, to the constant path it denotes: §6.5's (D-Index)/(D-Index-Trap) at each
dynamic step, "at the moment the path is navigated" (`7.1:10`). The steps are
taken left to right, each bounds-checked at the array it indexes before the
next is looked at; the index *values* were all computed before the first
check, which is the compiler's order too (probe r11: `a[id(5)][id(0)]` prints
`5` and `0`, then traps). Once resolved, the place is an ordinary constant
path, and §6.3's `readAt` and §6.8's `writeAt` take it from there (helper). -/
def Contents.resolveDyn : Contents → List Int → List (List Nat) → DynStep
  | _, [], [] => .ok []
  | .array _ _ cs, i :: is, π :: πs =>
      if inBoundsIdx i cs.length then
        (match cs[i.toNat]? with
         | none => .stuck .typeConfusion
         | some c =>
           match c.readAt π with
           | .error w => .stuck w
           | .ok c' =>
             match c'.resolveDyn is πs with
             | .ok ρ => .ok (i.toNat :: (π ++ ρ))
             | r => r)
      else .bounds
  | _, _, _ => .stuck .typeConfusion

/-- The index values of a dynamic place, as integers; `none` where one is not
an integer, which no well-typed program produces (helper). -/
def Val.ints : List Val → Option (List Int)
  | [] => some []
  | .int _ _ i :: vs =>
      match Val.ints vs with
      | some is => some (i :: is)
      | none => none
  | _ :: _ => none

/-- Where a place below a dynamic index lands in the store: the root's cell,
its contents, the contents at the constant place `p`, and the constant path
the dynamic tail resolved to under it — or the bounds trap, or a refusal
(helper). -/
inductive DynPlace where
  | at (ℓ : Nat) (c sub : Contents) (ρ : List Nat)
  | bounds
  | stuck (w : Violation)

/-- **Navigate a place below a dynamic index** (§6.3's `H(ℓ)@π` with §6.5's
bounds check at every dynamic step), once its index values `vs` are known.
One function for the read and the write, and for `eval` and its instrumented
mirror (`Explain.lean`), so the four test the same thing (helper). -/
def dynPlace (H : Store) (φ : Frame) (p : Place) (vs : List Val) (πs : List (List Nat)) :
    DynPlace :=
  match Val.ints vs with
  | none => .stuck .typeConfusion
  | some is =>
    match φ.env[p.root]? with
    | none => .stuck .unbound
    | some ℓ =>
      match H[ℓ]? with
      | none => .stuck .unbound
      | some .dead => .stuck .useAfterDrop
      | some (.full c) =>
        match c.readAt p.path with
        | .error w => .stuck w
        | .ok sub =>
          match sub.resolveDyn is πs with
          | .ok ρ => .at ℓ c sub ρ
          | .bounds => .bounds
          | .stuck w => .stuck w

/-- Whether the contents stored at a position is a struct **declared**
`linear`: the same mark `Ty.declaredLinear` (`Syntax.lean`) reads, read off the
declaration index §6.1's `{ v1, …, vk }_S` carries rather than off a type
(helper). -/
def Contents.declaredLinear (D : Decls) : Contents → Bool
  | .struct s _ _ =>
      (match D.structs[s]? with
       | some sd => sd.attr = .linear
       | none => false)
  | .hole | .array _ _ _ | .int _ _ _ | .float _ _ | .bool _ | .unit
  | .enum _ _ _ _ => false

/-- §6.3's use-plan annotation `μ`, recovered from the store: `some (π_d, π_s)`
where the path has a proper prefix of declared-`linear` struct type — §4.2's
`dl`, read on the stored aggregate — and `none` where it has none, which is the
`Ordinary` annotation the rules below fall to.

This is `declaredPrefix` (`Syntax.lean`) with the declaration index taken from
the value rather than from the type, clause for clause, and
`ContentsMatches.declaredPlan_eq` (`Soundness.lean`) is the proof that the two
agree wherever the store and Σ agree. The module docstring says why the plan is
recovered here rather than carried on the syntax as §6.3 writes it.

An **array** node is not a struct declared `linear`, so it offers no split of
its own and the walk continues into the element — the array clause of
`Ty.declaredLinear` (`Syntax.lean`), read on the value. -/
def Contents.declaredPlan (D : Decls) : Contents → List Nat → Option (List Nat × List Nat)
  | _, [] => none
  | .struct s i cs, f :: π =>
      (match cs[f]? with
       | some cf =>
           (match Contents.declaredPlan D cf π with
            | some r => some (f :: r.1, r.2)
            | none =>
                if (Contents.struct s i cs).declaredLinear D then some ([], f :: π) else none)
       | none =>
           if (Contents.struct s i cs).declaredLinear D then some ([], f :: π) else none)
  | .array _ _ cs, c :: π =>
      (match cs[c]? with
       | some ce =>
           (match Contents.declaredPlan D ce π with
            | some r => some (c :: r.1, r.2)
            | none => none)
       | none => none)
  | .hole, _ :: _ => none
  | .int _ _ _, _ :: _ | .float _ _, _ :: _ | .bool _, _ :: _ | .unit, _ :: _
  | .enum _ _ _ _, _ :: _ => none

/-- Evaluation results: a value with the final store and trace (§6.12's normal
result); a value handed back by an unwinding `return`, whose frame's scopes
have already been dropped (§6.9's (D-Return)) and which every enclosing form
passes on untouched until a call boundary absorbs it; a `break` on its way to
its loop, which every enclosing form passes on the same way until the loop
catches it and runs the drops it owes (§6.10's (D-Break)); a defined panic
(§6.12's `↯κ`); a violation ("stuck": either a configuration §6 leaves
undefined or a linear action one of the monitors refuses, named; the module
docstring says which is which); or exhausted fuel, which is not a machine
state at all but this interpreter's admission that it stopped early. -/
inductive EvalRes where
  | ok (H : Store) (v : Val) (tr : List Event)
  | returned (H : Store) (v : Val) (tr : List Event)
  /-- A `break` unwinding to its loop (§6.10's (D-Break)): the store, the
  scope record of the frame the `break` fired in — the loop reads off it which
  cells the body still owed a drop — and the trace so far. -/
  | broke (H : Store) (scope : List Nat) (tr : List Event)
  | panic (k : PanicKind) (tr : List Event)
  | stuck (why : Violation)
  | outOfFuel
deriving Repr

/-- Prefix a trace onto a result's trace. A `returned` result carries the
drops that ran before and during its unwind, so it takes the prefix exactly as
a normal result does (helper). -/
def EvalRes.withTrace (tr : List Event) : EvalRes → EvalRes
  | .ok H v tr' => .ok H v (tr ++ tr')
  | .returned H v tr' => .returned H v (tr ++ tr')
  | .broke H sc tr' => .broke H sc (tr ++ tr')
  | .panic k tr' => .panic k (tr ++ tr')
  | r => r

/-- §6.2's evaluation-context search, as a combinator: run an operand, and if
it reduced to a value, continue in the context with the store it left, the
operand's trace prefixed onto whatever the context produces. Every other
outcome — a trap (§6.12), a refusal, exhausted fuel, an unwinding `return`
(§6.9's (D-Return), which discards the context `E` it is under) and an
unwinding `break` ((D-Break) §6.10, which discards the context `E'` it is
under, pending `endscope` markers included) — is the whole form's outcome,
unchanged. -/
def EvalRes.andThen : EvalRes → (Store → Val → EvalRes) → EvalRes
  | .ok H v tr, k => (k H v).withTrace tr
  | r, _ => r

/-- §6.9's call boundary, as a combinator: the same search as `andThen`,
except that an unwinding `return` stops here. (D-Return) hands its value to
the suspended caller context, so at the one form that suspended a caller — a
call — a `returned` result becomes the call's value, with the drops its unwind
already ran. Everywhere else the `return` keeps travelling (`andThen`).

A `break` never crosses a call boundary: §5.7 makes one well-formed only
inside a loop, and (Fn) §5.8 gives a function body no `⟨break, _⟩` delivery,
so a callee's `break` is caught by a loop of its own body — "a `break` in a
callee would be ill-formed" (§6.10). One that reached the boundary anyway is
a configuration §6 leaves undefined, `typeConfusion`; `soundness` proves no
typed program reaches it. -/
def EvalRes.absorb : EvalRes → (Store → Val → EvalRes) → EvalRes
  | .ok H v tr, k => (k H v).withTrace tr
  | .returned H v tr, _ => .ok H v tr
  | .broke _ _ _, _ => .stuck .typeConfusion
  | r, _ => r

mutual
/-- `drop(H, c)` (§6.11), on the fragment's cell contents. A `⊘` drops
**nothing** — "this single skip is what makes double-free impossible" — and a
scalar drops nothing either ("scalars are Copy"; §7 says the same of a float —
it "has no drop glue, is never registered in a scope record, and never names
an allocation"). A struct runs its **user destructor first** (`3.9:28`), if its
declaration has one, and then drops its fields in **declaration order**
(`3.9:13`, §6.11's `drop*`), recursively. A field is dropped whatever its
class: an explicit `@drop` of a linear-carrying struct discharges the whole
obligation, and a scope exit never reaches a live linear sub-value, because the
leak monitor (`dropRetire`) reads `Contents.residualLinear` first.
`dropContents_struct_events`
(`Soundness.lean`) is this walk in closed form.

Two things §6.11 writes out are elided here, both unobservably.

* **The destructor is one `dtor` event, not a nested machine run.** The
  fragment has no destructor bodies — a declaration says only *whether* `S`
  has one — so there is nothing to step. The Rue program the printer emits
  supplies a body that reproduces the event (`Print.lean`).
* **The scratch cell is not minted.** §6.11 mints a fresh `ℓ` holding the
  value, runs the destructor in a frame whose scope record is empty, drops
  the *residual* fields `H1(ℓ)` leaves, and then retires `ℓ`. `dropContents`
  mints nothing and drops the original `cs`. Neither difference is
  observable: no `Event` corresponds to minting or retiring the scratch cell,
  and the residual fields *are* the original ones, because `3.9:33` forbids
  moving `self` out of a destructor and `3.9:34` forbids moving a field out
  of a value whose type declares one, so a destructor body cannot change a
  field. §6.11 says as much — it keeps the residual-versus-original
  distinction only so the rule stays honest if `3.9:34` is ever relaxed.

§6.11 is stated over cell contents, so its destructor case covers a
destructor-bearing struct one of whose fields is `⊘`. `3.9:34` makes that
state unreachable (no partial move may be taken under a destructor-bearing
value), so the walk runs the destructor on whatever the cell holds.
`Soundness.lean` proves the state is never reached.

§6.11's **enum** case (`6.3:20`) reads the stored tag and recurses into the
**active** variant's payload only, in payload order: an inactive variant's
payload has no storage, and a discriminant-only active variant drops nothing
because its payload list is empty (probe e1b). An enum runs no destructor of its
own — §3 gives it none to declare (E0417) — so, unlike the struct case, there is
no event before the payload's and no declaration to look up, which is why this
arm cannot refuse and why the value's enum **index** is not read: there is
nothing to look it up for. Under `Typed` it is pinned anyway
(`HasTy.enum_inv`). A payload already moved out by a `match` binding left the enum
place `⊘` and is skipped by the `⊘` case above, never dropped twice. -/
def dropContents (D : Decls) : Contents → Except Violation (List Event)
  | .hole => .ok []
  | .int _ _ _ => .ok []
  | .float _ _ => .ok []
  | .bool _ => .ok []
  | .unit => .ok []
  | .struct s i cs =>
      match D.structs[s]? with
      | none => .error .unbound
      | some sd =>
          match dropContentsList D cs with
          | .error w => .error w
          | .ok evs =>
              .ok ((if sd.dtor then [Event.dtor s (.struct s i cs)] else []) ++ evs)
  | .enum _ _ _ cs => dropContentsList D cs
  -- §6.11's `drop(H, [v1,…,vn]) = drop*(H, [v1,…,vn])`: an array declares no
  -- destructor of its own — `3.9:14` gives `[T; n]` one exactly when `T` has
  -- one — so the walk is the elements' own, in **ascending index order**
  -- (`3.9:15`, `3.8:73`), every `⊘` skipped.
  | .array _ _ cs => dropContentsList D cs

/-- `drop*(H, [c1,…,ck])` (§6.11): fold `drop` over the contents left to right
— for a struct's fields, declaration order (`3.9:13`); for an array's
elements, ascending index order (`3.9:15`). -/
def dropContentsList (D : Decls) : List Contents → Except Violation (List Event)
  | [] => .ok []
  | c :: cs =>
      match dropContents D c with
      | .error w => .error w
      | .ok evs =>
          match dropContentsList D cs with
          | .error w => .error w
          | .ok evs' => .ok (evs ++ evs')
end

mutual
/-- **§6.11's order, as a function**: the events dropping a cell's contents
emits, written out rather than read off the walk. A `⊘` and a scalar emit none;
a struct emits its user destructor's event first when its declaration has one
(`3.9:28`) and then its fields' events in declaration order (`3.9:13`),
recursively, every `⊘` skipped; an enum emits exactly its **active** variant's
payload's events, in payload order, and none at all for a discriminant-only
variant (`6.3:20`). An index the environment does not have emits
nothing, which the walk itself refuses instead —
`dropContents_struct_events` (`Soundness.lean`) is the theorem that the two
agree on every well-typed contents, and it is the closed form RUE-2237's
"dropped exactly once" quantifies over. -/
def dropEvents (D : Decls) : Contents → List Event
  | .hole => []
  | .int _ _ _ => []
  | .float _ _ => []
  | .bool _ => []
  | .unit => []
  | .struct s i cs =>
      (match D.structs[s]? with
       | some sd => if sd.dtor then [Event.dtor s (.struct s i cs)] else []
       | none => []) ++ dropEventsList D cs
  | .enum _ _ _ cs => dropEventsList D cs
  | .array _ _ cs => dropEventsList D cs

/-- The same over a field, payload or element list: the members' events
concatenated in declaration order (`3.9:13`) or ascending index order
(`3.9:15`), which is §6.11's `drop*`. -/
def dropEventsList (D : Decls) : List Contents → List Event
  | [] => []
  | c :: cs => dropEvents D c ++ dropEventsList D cs
end

mutual
/-- **§6.3's `split(H(ℓ)@π_d, π_s) = (v, [r_1, …, r_m])`**: expose the selected
leaf and the ordered unselected residue. "`split` walks structs in declaration
order[…]; at each selected step it recurses, and at each unselected step it
appends the whole value", so the residue of a step is *the fields before the
selected one*, then *whatever the recursion into it retained*, then *the fields
after it* — nested residue before a later sibling (probe d14), and plain
declaration order where the leaf is a direct field (probe d13).

An empty path selects the whole aggregate and retains nothing: the leaf "is not
residue". A `⊘` with path left to walk is `useAfterMove`, as `readAt`'s is; a
step that is not a field of what is stored is a shape no well-typed program
produces. An **array** step is §5.1's own clause — "at an array step, visit
elements in ascending constant-index order, recurse into the selected element,
and retain every unselected element" — and it is the struct step's walk over a
different list, so it runs the same `splitFields`. Probe `b20` pins the order
on the compiler: a declared-`linear` `{ p, arr: [S1; 3], q }` destructured at
`x.arr[1]` drops `p`, `arr[0]`, `arr[2]`, `q`, in that order. -/
def Contents.splitResidue (D : Decls) :
    Contents → List Nat → Except Violation (Contents × List Contents)
  | c, [] => .ok (c, [])
  | .struct _ _ cs, f :: π => Contents.splitFields D cs f π
  | .array _ _ cs, c :: π => Contents.splitFields D cs c π
  | .hole, _ :: _ => .error .useAfterMove
  | .int _ _ _, _ :: _ | .float _ _, _ :: _ | .bool _, _ :: _ | .unit, _ :: _
  | .enum _ _ _ _, _ :: _ => .error .typeConfusion

/-- `split`'s step over one node's stored members — a declaration's fields, or
an array's elements: retain the members before the selected slot, recurse into
it, and retain the members after — which is §6.3's "visit fields in declaration
order" (and §5.1's ascending-index order at an array) written as a structural
recursion rather than as a `take`/`drop` (helper). -/
def Contents.splitFields (D : Decls) :
    List Contents → Nat → List Nat → Except Violation (Contents × List Contents)
  | [], _, _ => .error .typeConfusion
  | c :: cs, 0, π =>
      (match Contents.splitResidue D c π with
       | .error w => .error w
       | .ok (leaf, inner) => .ok (leaf, inner ++ cs))
  | c :: cs, f + 1, π =>
      (match Contents.splitFields D cs f π with
       | .error w => .error w
       | .ok (leaf, rest) => .ok (leaf, c :: rest))
end

/-- **The residue marker** (RUE-2427): a retained subtree `r` of the cell `ℓ`
being destructured is a sub-position of `ℓ` being dropped, which is exactly
what `@drop(x.f)` records as `drop ℓ sub`, so its drop starts with the same
`drop ℓ r` marker — when `r` is not `Copy`, as `dropCell` records one. A
`Copy` subtree has no drop glue and gets no marker (helper). -/
def residueMark (D : Decls) (ℓ : Nat) (r : Contents) : List Event :=
  if r.mult D = .copy then [] else [.drop ℓ r]

/-- §6.3's `drop*` applied to `[r_1, …, r_m]` **left to right**, so "each
legally droppable residue is destroyed immediately and exactly once". Each
element's drop is its marker (`residueMark`) and then §6.11's walk of it.

The `residualLinear` test is the monitor this machine adds and §6.3 does not
need: §5.1's `¬ linear-residue(S, π_s)` premise has already excluded a linear
residue before the redex fires, so on a program `check` accepts the branch is
unreachable (`ContentsMatches.destructure_ok`, `Soundness.lean`). On a program
`check` rejects it turns the silent destruction of a linear value into a named
refusal, which is what `3.8:60` (E0474) is about.

The test is per element, immediately before that element's own drop, which is
`unwindLocs`' shape at a scope record rather than `dropRetire`'s at one cell.
Nothing is destroyed early by it: `dropContents` writes no store, the `⊘` at
`ℓ@π_d` is the caller's step *after* `destructure` returns `.ok`, and a
refusal discards the events, so an earlier residue's drop leaves no trace and
no heap effect behind. -/
def dropResidue (D : Decls) (ℓ : Nat) : List Contents → Except Violation (List Event)
  | [] => .ok []
  | r :: rs =>
      if r.residualLinear D then .error .linearLeak
      else
        match dropContents D r with
        | .error w => .error w
        | .ok evs =>
            match dropResidue D ℓ rs with
            | .error w => .error w
            | .ok evs' => .ok (residueMark D ℓ r ++ evs ++ evs')

mutual
/-- **The consumed shell of a destructure** (RUE-2427): the nodes on the
selected path — the declared-`linear` place `d` and every node below it down
to the leaf's parent — with the leaf and every retained subtree replaced by
`⊘`. It is what §6.3's destructure consumes without dropping: the leaf is
handed on, the residue is dropped, and `ℓ@π_d` becomes `⊘`. It walks the path
exactly as `splitResidue` does (helper). -/
def Contents.skeleton : Contents → List Nat → Contents
  | _, [] => .hole
  | .struct s i cs, f :: π => .struct s i (Contents.skelFields cs f π)
  | .array T i cs, f :: π => .array T i (Contents.skelFields cs f π)
  | _, _ :: _ => .hole

/-- `skeleton`'s member step: `⊘` at every unselected slot, the recursion at
the selected one (helper). -/
def Contents.skelFields : List Contents → Nat → List Nat → List Contents
  | [], _, _ => []
  | c :: cs, 0, π => Contents.skeleton c π :: cs.map (fun _ => .hole)
  | _ :: cs, f + 1, π => .hole :: Contents.skelFields cs f π
end

/-- **§6.3's `destructure(H, ℓ@π_d, π_s)`**, on the contents stored at the
consumed place of cell `ℓ`: `split` the aggregate, then apply `drop*` to the
residue, then record the consumption of the path's shell (`consume`,
RUE-2427). The result is the selected leaf — "the result transferred to the
context, not a value dropped by `destructure`" — and the residue's drop events
followed by the consumption. Writing `⊘` at `ℓ@π_d` is the caller's step,
because §6.3 puts it *after* the residue's drops. -/
def Contents.destructure (D : Decls) (ℓ : Nat) (c : Contents) (πs : List Nat) :
    Except Violation (Contents × List Event) :=
  match Contents.splitResidue D c πs with
  | .error w => .error w
  | .ok (leaf, rs) =>
      match dropResidue D ℓ rs with
      | .error w => .error w
      | .ok evs => .ok (leaf, evs ++ [.consume (c.skeleton πs)])

/-- **The residue's trace, in closed form**: each retained subtree's marker and
§6.11's events, in the traversal's own order. `dropResidue_events`
(`Soundness.lean`) is the theorem that `dropResidue` emits exactly this on
well-typed residue, which is what keeps the drop-order statements closed under
the new redex. -/
def dropResidueEvents (D : Decls) (ℓ : Nat) (rs : List Contents) : List Event :=
  rs.flatMap (fun r => residueMark D ℓ r ++ dropEvents D r)

/-- A `match` consumes a non-`Copy` scrutinee's **shell** (RUE-2427): the enum
node, its payload already bound to the arm's cells. The event names the node
with every payload slot `⊘`. A `Copy` scrutinee was copied, not consumed, and
its shell owns nothing, so nothing is recorded (helper). -/
def matchConsume (D : Decls) (e k i : Nat) (vs : List Val) : List Event :=
  if D.enumClassOf e = .copy then [] else [.consume (.enum e k i (vs.map fun _ => .hole))]

/-- `@dbg`'s operand domain: the values §6.12's rendering is defined on —
an integer, a float or a `bool` (§5.8's (Dbg) types the operand
`Ty.observable`; the compiler accepts integer, `bool` and `String` and nothing
else) (helper). -/
def Val.observable : Val → Bool
  | .int _ _ _ | .float _ _ | .bool _ => true
  | .unit | .struct _ _ _ | .enum _ _ _ _ | .array _ _ _ => false

/-- A field list's events are its fields' events concatenated, left to right:
the flattening `dropContents_struct_events` states the order with (helper). -/
theorem dropEventsList_eq_flatten (D : Decls) :
    ∀ cs : List Contents, dropEventsList D cs = (cs.map (dropEvents D)).flatten
  | [] => rfl
  | c :: cs => by simp [dropEventsList, dropEventsList_eq_flatten D cs]

/-- The drop of a binding cell's contents (§6.11), as the trace records it: a
`drop ℓ c` marker naming the cell, then the events the contents' own drop
emits. `Copy` contents — a scalar, or a `⊘` with nothing left in it — has no
drop glue at all (§6.11: `drop(H, n_T) = H`, `drop(H, ⊘) = H`), so it records
nothing, which is also why `@drop` of a `Copy` place leaves no trace (§5.3's
(@Drop-Copy)) (helper). -/
def dropCell (D : Decls) (ℓ : Nat) (c : Contents) : Except Violation (List Event) :=
  if c.mult D = .copy then .ok []
  else
    match dropContents D c with
    | .error w => .error w
    | .ok evs => .ok (.drop ℓ c :: evs)

/-- `drop-retire(H, ℓ)` (§6.1): run the binding's drop (§6.11 — a no-op on a
`⊘` or `Copy` cell), then retire the allocation, so any later access to it is
`useAfterDrop` rather than silently readable (the RUE-390 change). A live
linear value here is §5.6's leak: the scope ends with an obligation
undischarged, and the machine refuses (`3.8:32`). The monitor reads
`Contents.residualLinear`, §5.6's own recursion on the store's side, because
after a partial move the obligation attaches to whatever linear content is
still present rather than to the binding's type (RUE-1591). This is the one
scope-teardown path: `let`'s normal `endscope` (§6.7) and the frame unwind of
`return` (§6.9) both run it. -/
def dropRetire (D : Decls) (H : Store) (ℓ : Nat) : Except Violation (Store × List Event) :=
  match H[ℓ]? with
  | none => .error .unbound
  | some .dead => .error .useAfterDrop
  | some (.full c) =>
      if c.residualLinear D then .error .linearLeak
      else
        match dropCell D ℓ c with
        | .error w => .error w
        | .ok evs => .ok (H.set ℓ .dead, evs)

/-- `run-scope-drops` (§6.1): drop-retire a scope's cells in the order given,
accumulating the drop events. Callers pass the record newest-first, which is
the order §6.1 fixes for a scope's teardown (RAII). -/
def unwindLocs (D : Decls) (H : Store) : List Nat → Except Violation (Store × List Event)
  | [] => .ok (H, [])
  | ℓ :: rest =>
      match dropRetire D H ℓ with
      | .error w => .error w
      | .ok (H₁, evs) =>
          match unwindLocs D H₁ rest with
          | .error w => .error w
          | .ok (H₂, evs') => .ok (H₂, evs ++ evs')

/-- `run-all-scope-drops(H, φ)` (§6.1, §6.9): the whole-frame teardown, run
when a frame is popped — at a normal (D-Return-Value) and at an unwinding
(D-Return). The frame's record lists its cells in creation order, so the
teardown reads it backwards: newest binding first. -/
def runAllScopeDrops (D : Decls) (H : Store) (φ : Frame) :
    Except Violation (Store × List Event) :=
  unwindLocs D H φ.scope.reverse

/-- (D-Call) §6.9: mint one fresh single-cell binding allocation per by-value
argument, left to right, each holding its argument's value. Returns the store
and the locations in creation order — the callee's entry scope record, which
owes a drop for exactly these cells. The callee's environment is its reverse,
because `Env` (like `Ctx`) lists the innermost binder first and the last
parameter is the innermost. -/
def mintParams : Store → List Val → Store × List Nat
  | H, [] => (H, [])
  | H, v :: vs =>
      let (H', locs) := mintParams (H ++ [.full (Contents.ofVal v)]) vs
      (H', H.length :: locs)

/-- The outcome of evaluating an argument list: the store, the argument values
in order and the trace, or the non-`ok` result of the first argument that did
not produce one, passed on unchanged (§6.2's left-to-right search through
`g(v̄, …, E, …)`) (helper). -/
inductive ArgsRes where
  | ok (H : Store) (vs : List Val) (tr : List Event)
  | abort (r : EvalRes)

/-- Evaluate a call's by-value arguments left to right, threading the store
(§6.2's evaluation order, §6.9's by-value argument rule). `ev` is the
interpreter at the fuel the caller has already spent one unit of, which is
what keeps `eval` structurally recursive on its fuel. A `returned` argument
aborts the call: its frame has already unwound, and no parameter cell was
minted (helper). -/
def evalArgs (ev : Store → Expr → EvalRes) : Store → List Expr → ArgsRes
  | H, [] => .ok H [] []
  | H, e :: es =>
      match ev H e with
      | .ok H₁ v tr =>
          (match evalArgs ev H₁ es with
           | .ok H₂ vs tr₂ => .ok H₂ (v :: vs) (tr ++ tr₂)
           | .abort r => .abort (r.withTrace tr))
      | r => .abort r

/-! ## §6.4's primitive operators

Every rule of §6.4 the fragment reaches is here, computed over `Int` and
range-checked against the operand type, which is what `arith + range_check`
means: the exact result is formed first and the trap is the check on it
(`3.1:6` — Rue arithmetic never wraps). The bitwise and shift rules compute
over the `w`-bit pattern instead (`valOf`/`bitsOf`, `Syntax.lean`) and are
total by construction.
-/

/-- What a primitive operator produced: a value, one of §6.12's traps, or a
refusal because an operand was the wrong shape — which the statics exclude and
`soundness` proves they do (helper). -/
inductive OpRes where
  | val (v : Val)
  | trap (k : PanicKind)
  | confused
deriving Repr

/-- `range_check` (§6.4): an exact integer result becomes a value of
`int(w,s)` when it lies in `[min_T, max_T]` and `↯overflow` when it does not —
(D-Arith) and (D-Arith-Trap) in one (helper). -/
def intResult (w : IntWidth) (s : Sign) (n : Int) : OpRes :=
  if InBounds w s n then .val (.int w s n) else .trap .overflow

/-- The shift amount, reduced modulo the operand width (`k = amt mod w`,
§6.4's (D-Shl)/(D-Shr)). The reduction is **Euclidean** — `Int.emod` returns
the representative in `[0, w)` — which is what §6.4 fixes `mod` to, and it is
the clause that decides a *negative* amount. `4.3a:9` gives the amount the
shifted value's own type, so on a signed type a negative one is writable, and
`4.3a:10` covers only an amount "greater than or equal to the bit width": it
is §6.4, not `4.3a:10`, that this reads. Shifting never traps (helper). -/
def shiftAmount (w : IntWidth) (n : Int) : Nat := (n % (w.bits : Int)).toNat

/-- §6.4's binary integer rules at one `int(w,s)`, on the two operands'
values:

* `+ - *` are (D-Arith)/(D-Arith-Trap);
* `/` is (D-Div)/(D-Div-Zero)/(D-Div-Overflow) — truncated toward zero, and
  the `min_T / -1` case is exactly the one the range check rejects;
* `%` is the remainder arm of the same group: `↯rem-zero` on a zero divisor
  and `↯overflow` on `min_T % -1`, which §6.4 traps even though the
  mathematical remainder is `0`, so the case is written out rather than left
  to the range check;
* `& | ^` are (D-Bit) and `<< >>` are (D-Shl)/(D-Shr), over the `w`-bit
  pattern, with `>>` arithmetic on a signed type and logical on an unsigned
  one; none of them traps;
* `< <= > >=` are §6.4's `cmp`, on the integer value, which carries its own
  sign. -/
def binOpInt (op : BinOp) (w : IntWidth) (s : Sign) (n₁ n₂ : Int) : OpRes :=
  match op with
  | .add => intResult w s (n₁ + n₂)
  | .sub => intResult w s (n₁ - n₂)
  | .mul => intResult w s (n₁ * n₂)
  | .div => if n₂ = 0 then .trap .divZero else intResult w s (n₁.tdiv n₂)
  | .rem =>
      if n₂ = 0 then .trap .remZero
      else if s = .signed ∧ n₁ = intMin w s ∧ n₂ = -1 then .trap .overflow
      else intResult w s (n₁.tmod n₂)
  | .bitAnd => .val (.int w s (valOf w s (bitsOf w n₁ &&& bitsOf w n₂)))
  | .bitOr => .val (.int w s (valOf w s (bitsOf w n₁ ||| bitsOf w n₂)))
  | .bitXor => .val (.int w s (valOf w s (bitsOf w n₁ ^^^ bitsOf w n₂)))
  | .shl => .val (.int w s (valOf w s (bitsOf w n₁ * 2 ^ shiftAmount w n₂)))
  | .shr =>
      match s with
      | .unsigned => .val (.int w s (valOf w s (bitsOf w n₁ / 2 ^ shiftAmount w n₂)))
      | .signed => .val (.int w s (wrapInt w s (Int.fdiv n₁ (2 ^ shiftAmount w n₂))))
  | .lt => .val (.bool (decide (n₁ < n₂)))
  | .le => .val (.bool (decide (n₁ ≤ n₂)))
  | .gt => .val (.bool (decide (n₂ < n₁)))
  | .ge => .val (.bool (decide (n₂ ≤ n₁)))
  -- `@total_cmp` has no integer rule: `3.12:31` gives it two float operands,
  -- and `BinOp.intAdmits` (§5.8) excludes it, so this arm is unreachable for
  -- a well-typed program. The machine names the shape rather than inventing
  -- an integer total order.
  | .totalCmp => .confused

/-- §6.4's float rules at one `float(w)`, on the two operands' data.

* `+ - * /` are `(D-Float-Arith)`: `f₁ ⊕_w f₂`, the model's, and **total** —
  no float arithmetic redex steps to a panic (`3.12:21`, and §6.4's note that
  none of (D-Arith-Trap), (D-Div-Zero) or (D-Div-Overflow) is stated over a
  float redex);
* `< <= > >=` are `(D-Float-Ord)`: the IEEE 754 predicate, `false` whenever
  either operand is a NaN (`3.12:27`) and equal on the two zeros (`3.12:28`);
* `@total_cmp` is `(D-Total-Cmp)`: `≺_w`, a **total** order, `0` exactly on
  the same datum;
* `%` and the bitwise and shift operators have no float rule at all — §5.8
  rejects them by the absence of one (`3.12:25`) — so the machine refuses
  them, as it does two operands of different types. -/
def binOpFloat (M : FloatOps) (op : BinOp) (w : FloatWidth) (a b : FloatDatum) : OpRes :=
  match op with
  | .add => .val (.float w (M.arith w .add a b))
  | .sub => .val (.float w (M.arith w .sub a b))
  | .mul => .val (.float w (M.arith w .mul a b))
  | .div => .val (.float w (M.arith w .div a b))
  | .lt => .val (.bool (a.lt b))
  | .le => .val (.bool (a.le b))
  | .gt => .val (.bool (b.lt a))
  | .ge => .val (.bool (b.le a))
  | .totalCmp => .val (.int .w32 .signed (a.totalCmp b))
  | .rem | .bitAnd | .bitOr | .bitXor | .shl | .shr => .confused

/-- §6.4's binary operators on two machine values. §5.8's rules give both
operands one `int(w,s)` or one `float(w)`, so operands of two different types
— two integer types, two float widths (`3.12:13`: no implicit widening), or
one of each (`3.12:14`) — are a shape no well-typed program produces and the
machine refuses them. -/
def evalBinOp (M : FloatOps) (op : BinOp) : Val → Val → OpRes
  | .int w₁ s₁ n₁, .int w₂ s₂ n₂ =>
      if w₁ = w₂ ∧ s₁ = s₂ then binOpInt op w₁ s₁ n₁ n₂ else .confused
  | .float w₁ f₁, .float w₂ f₂ =>
      if w₁ = w₂ then binOpFloat M op w₁ f₁ f₂ else .confused
  | _, _ => .confused

/-- §6.4's unary operators. `neg` is (D-Arith)'s unary case on an integer,
trapping on `min_T` because `-min_T > max_T`, and `(D-Float-Neg)` on a float,
which is **total**: a sign flip and nothing else, on `-0.0` and on a NaN
alike (`3.12:24`). `not` on `bool` is total (§6.4's `Not`); `bitnot` inverts
the `w`-bit pattern ((D-Bit)'s complement arm) and is total too. §5.8
restricts the integer `neg` to a signed operand, so the unsigned case below is
a shape no well-typed program produces; it is written as the same range check
rather than as a refusal, because the exact result `-n` is what §6.4 computes
and the check is what decides. -/
def evalUnOp : UnOp → Val → OpRes
  | .neg, .int w s n => intResult w s (-n)
  | .neg, .float w f => .val (.float w f.negate)
  | .not, .bool b => .val (.bool (!b))
  | .bitnot, .int w s n => .val (.int w s (valOf w s (w.modulus - 1 - bitsOf w n)))
  | _, _ => .confused

/-- §6.4's one-operand float intrinsics.

* `@int_to_float` is `(D-Int-To-Float)`: `rnd_w` of the operand's exact
  integer value, the model's, and it never traps (`3.12:16`);
* `@float_to_int` is `(D-Float-To-Int)` **and** `(D-Float-To-Int-Trap)`, the
  one float form that traps: the operand truncated toward zero when that is
  defined and in the target's range, and `↯overflow` otherwise — the same
  category §6.12 already lists, not a new one (`3.12:18`, `8.1:7`), which is
  why the compiler reports it as `integer overflow` (verified by hand). The
  two arms **partition** `𝔽_w` (`floatToInt_partition`, `Float.lean`), which
  is what keeps progress intact here;
* `@float_cast` is `(D-Float-Cast)`: exact widening, the model's `rnd_32`
  narrowing, never trapping (`3.12:19`);
* the five `3.12:34` intrinsics are `(D-Float-Round)`: `@sqrt` is the model's,
  the other four are exact, and none traps (`3.12:37`). -/
def evalFintrin (M : FloatOps) : FloatIntrin → Val → OpRes
  | .intToFloat w, .int _ _ n => .val (.float w (M.ofInt w n))
  | .floatToInt w' s', .float _ f =>
      match f.toIntIn (intMin w' s') (intMax w' s') with
      | some t => .val (.int w' s' t)
      | none => .trap .overflow
  | .floatCast w', .float w f => .val (.float w' (M.cast w w' f))
  | .roundOp k, .float w f => .val (.float w (M.roundIntrin w k f))
  | _, _ => .confused

/-- `@intCast` (`4.13:28`): the value survives when it denotes a value of the
target type, and traps when it does not. The conversion is on the *value*, not
on the bit pattern — `@bitCast` is the reinterpreting intrinsic and is not in
the fragment. -/
def evalIntCast (w : IntWidth) (s : Sign) : Val → OpRes
  | .int _ _ n => if InBounds w s n then .val (.int w s n) else .trap .castOverflow
  | _ => .confused

/-- An operator's outcome as an evaluation result, at the store the operands
left: a value carries no events, a trap carries none of its own (the events
the operands emitted are prefixed by `andThen`), and a refusal is named
(helper). -/
def OpRes.toRes (H : Store) : OpRes → EvalRes
  | .val v => .ok H v []
  | .trap k => .panic k []
  | .confused => .stuck .typeConfusion

/-- **Aggregate introduction mints a value identity** ((D-Struct) and (D-Array)
§6.5, (D-Enum-Intro) §6.6, and the repeat form): the new value's identity is
`H.length`, the next index of the store, and that index is reserved by
appending `†`. §6.1 draws every identity from one pool — "a fresh identity is
one not in `dom(H)`; dead allocations stay in the domain as `†`, so an
identity is never reused" — so a reserved index is a name no later mint, and
no later binding, can take. The reserved slot holds no storage and nothing
binds it; it is never read. The identity travels with the value — through
`Contents.ofVal`/`toVal` into and out of cells, into a callee's parameter
cells and a `match` arm's payload cells — and the drop trace records it
(`Event`), which is what `no_double_free` (`Trace.lean`) counts.

The copy-closure monitor (`Contents.copyClosed`) runs here, on the finished
value (helper). -/
def introVal (D : Decls) (H : Store) (mk : Nat → Val) : EvalRes :=
  if (Contents.ofVal (mk H.length)).copyClosed D then .ok (H ++ [.dead]) (mk H.length) []
  else .stuck .ownedUnderCopy

/-- The interpreter, over a `FloatOps` (`Float.lean`): §2 fixes `rnd_w` and
`σ_NaN` per *target*, not per rule, so the machine takes them as a parameter
and every theorem quantifies over a model that satisfies §7's laws. Rule
correspondence, per case: `use` is
(D-Use-Declared-Linear)/(D-Use-Copy)/(D-Use-Move) (§6.3), branching on the use
plan before anything else; `binop`, `unop`, `intCast` and
`fintrin` are §6.4's operator and intrinsic rules, computed by
`evalBinOp`/`evalUnOp`/`evalIntCast`/`evalFintrin` above, the float half of
them through the model `M`;
`panic` is (D-Panic) §6.12; `dbg` appends the operand's rendering to the
observable output (§5.8's (Dbg), §6.12's `Outcome`); `drop` is §6.11's
explicit `@drop`; `letIn` is (D-Let) + (D-EndScope)'s drop-retire (§6.7);
`assign` is (D-Assign), §6.8's overwrite-drop / reinitialization, with the
copy-closure monitor on what it stores; `seq` is
(D-Seq), discarding with a temporary drop (§6.7); `mkStruct` is (D-Struct)
§6.5 after §6.2's left-to-right search through its initializers and `mkArray`
is (D-Array) §6.5 after the same search, each minting its value's identity
(`introVal`); `repeatArray` is §2's elaboration of
the surface repeat form (`7.1:39`); `indexRead` and `indexWrite` are
(D-Index)/(D-Index-Trap) §6.5 at a dynamic index, reading by §6.3's copy rule
and writing by §6.8's overwrite; `mkEnum` is
(D-Enum-Intro) §6.6 after the same search, and `«match»` is (D-Match) §6.6 —
the tag switch, the payload cells bound as (D-Let) binds one, and their
newest-first drop at the arm's end; `ite` is (D-If-T)/(D-If-F) after the §6.2
search for the scrutinee; `call` is (D-Call)
followed by (D-Return-Value) when the body completes normally, and by
(D-Return)'s absorption when it does not; `ret` is (D-Return), which runs the
frame's scope drops and hands the value past every enclosing form; `loop` is
(D-Loop-Enter) and (D-Loop-Iter) §6.10, re-entering the body at one unit of
fuel less after every turn that completes, and (D-Break)'s unwind when the
body breaks; `brk` is (D-Break), which hands its loop the frame's scope
record.

Every operand is sequenced with `andThen`, which is §6.2's search through an
evaluation context; the callee's body is sequenced with `absorb`, the one
place a `return` stops travelling (§6.9). -/
def eval (M : FloatOps) : Nat → Program → Store → Frame → Expr → EvalRes
  | 0, _, _, _, _ => .outOfFuel
  | _ + 1, _, H, _, .intLit w s n => .ok H (.int w s n) []
  | _ + 1, _, H, _, .floatLit w l =>
      -- (Lit) at `float(w)`: `3.12:9` reads the literal's decimal at the
      -- form's width, which is `rnd_w` — the model's.
      .ok H (.float w (M.ofLit w l.sig l.negExp l.e)) []
  | _ + 1, _, H, _, .boolLit b => .ok H (.bool b) []
  | _ + 1, _, H, _, .unitLit => .ok H .unit []
  | _ + 1, P, H, φ, .use p =>
      -- (D-Use-Declared-Linear)/(D-Use-Copy)/(D-Use-Move) §6.3: resolve the
      -- root under ρ, then branch on the use plan. With a `Declared(d, π_s)`
      -- plan the destructure is its own redex — split, drop the residue, and
      -- write `⊘` at the *consumed place* `ℓ@π_d`; otherwise navigate the path
      -- and, for a non-`Copy` place, write `⊘` at exactly that sub-position,
      -- which is the partial move of §4.2.
      match φ.env[p.root]? with
      | none => .stuck .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => .stuck .unbound
        | some .dead => .stuck .useAfterDrop
        | some (.full c) =>
          match c.declaredPlan P.decls p.path with
          | some (πd, πs) =>
            (match c.readAt πd with
             | .error w => .stuck w
             | .ok cd =>
               match cd.destructure P.decls ℓ πs with
               | .error w => .stuck w
               | .ok (leaf, evs) =>
                 match leaf.toVal with
                 | none => .stuck .useAfterMove
                 | some v =>
                   match c.writeAt πd .hole with
                   | none => .stuck .typeConfusion
                   | some c' => .ok (H.set ℓ (.full c')) v evs)
          | none =>
            match c.readAt p.path with
            | .error w => .stuck w
            | .ok sub =>
              match sub.toVal with
              | none => .stuck .useAfterMove
              | some v =>
                  if v.mult P.decls = .copy then .ok H v []
                  else
                    match c.writeAt p.path .hole with
                    | none => .stuck .typeConfusion
                    | some c' => .ok (H.set ℓ (.full c')) v []
  | fuel + 1, P, H, φ, .binop op e₁ e₂ =>
      (eval M fuel P H φ e₁).andThen fun H₁ v₁ =>
        (eval M fuel P H₁ φ e₂).andThen fun H₂ v₂ =>
          (evalBinOp M op v₁ v₂).toRes H₂
  | fuel + 1, P, H, φ, .unop op e =>
      (eval M fuel P H φ e).andThen fun H' v => (evalUnOp op v).toRes H'
  | fuel + 1, P, H, φ, .intCast w s e =>
      (eval M fuel P H φ e).andThen fun H' v => (evalIntCast w s v).toRes H'
  | fuel + 1, P, H, φ, .fintrin k e =>
      (eval M fuel P H φ e).andThen fun H' v => (evalFintrin M k v).toRes H'
  | _ + 1, _, _, _, .panic _ =>
      -- (D-Panic) §6.12: the message is emitted and the configuration is
      -- abandoned. No scope drop runs — §5.7 exempts the `⊥_panic` edge from
      -- §5.6's obligation — so the trace this trap carries is exactly the one
      -- the evaluation had already produced, prefixed by `andThen`.
      .panic .user []
  | fuel + 1, P, H, φ, .dbg e =>
      -- The operand must be one §6.12 can render (`Val.observable`): §6 has no
      -- rule that appends the rendering of anything else, so the machine is
      -- stuck rather than silently discarding an owned operand (RUE-2427). A
      -- checked program never reaches it: (Dbg) §5.8 types the operand
      -- `Ty.observable` (`soundness`).
      (eval M fuel P H φ e).andThen fun H' v =>
        if v.observable then .ok H' .unit [.dbg v] else .stuck .typeConfusion
  | fuel + 1, P, H, φ, .mkStruct s args =>
      -- (D-Struct) §6.5: a struct literal is a redex once every initializer
      -- is a value; §6.2's contexts reduce them left to right, threading `H`,
      -- exactly as a call's arguments are reduced (§6.9).
      (match evalArgs (fun H' e => eval M fuel P H' φ e) H args with
       | .abort r => r
       | .ok H₁ vs tr =>
         EvalRes.withTrace tr <|
           match P.decls.structs[s]? with
           | none => .stuck .unbound
           | some sd =>
               if sd.fields.length = vs.length then introVal P.decls H₁ (fun i => .struct s i vs)
               else .stuck .typeConfusion)
  | fuel + 1, P, H, φ, .mkEnum e k args =>
      -- (D-Enum-Intro) §6.6: an enum literal is a redex once every payload
      -- component is a value; §6.2's contexts reduce them left to right
      -- (`Kj( v1, …, E, … )`), exactly as a struct literal's fields are, and
      -- the result is §6.1's tagged value `Kj⟨v1,…,va⟩` — the bare tag when the
      -- variant is discriminant-only.
      (match evalArgs (fun H' e' => eval M fuel P H' φ e') H args with
       | .abort r => r
       | .ok H₁ vs tr =>
         EvalRes.withTrace tr <|
           match P.decls.enums[e]? with
           | none => .stuck .unbound
           | some ed =>
             match ed.variants[k]? with
             | none => .stuck .typeConfusion
             | some Ts =>
                 if Ts.length = vs.length then introVal P.decls H₁ (fun i => .enum e k i vs)
                 else .stuck .typeConfusion)
  | fuel + 1, P, H, φ, .«match» scrut arms =>
      -- (D-Match) §6.6: reduce the scrutinee to an enum value — a *use* of its
      -- place, so a non-`Copy` enum's cell became `⊘` by §6.3 and a `Copy` one
      -- was read — then read the tag, which selects the one covering arm
      -- (exhaustiveness, §5.5, makes `arms[k]?` succeed for a well-typed value:
      -- `exhaustive_arm_exists`, `Soundness.lean`). The value's enum **index**
      -- is read only to class the consumed shell (`matchConsume`): the arms
      -- come from the `match` form, and under `Typed` the index is the
      -- scrutinee's own (`HasTy.enum_inv`).
      (eval M fuel P H φ scrut).andThen fun H₀ v =>
        match v with
        | .enum e k i vs =>
          (match arms[k]? with
           | none => .stuck .typeConfusion
           | some body =>
             -- The payload components are bound to fresh cells exactly as
             -- (D-Let) §6.7 binds one: appended to the frame's innermost scope
             -- record *and* owed to the arm's `endscope([ℓ1,…,ℓa])` marker,
             -- which the arm's normal path below runs. `mintParams` is the same
             -- minting (D-Call) §6.9 performs, and the two `reverse`s are the
             -- same one it needs: a payload tuple is written left to right while
             -- `Env` lists the innermost binder first.
             -- The scrutinee's shell is consumed here, before the arm runs
             -- (`matchConsume`, RUE-2427): its payload now lives in the cells.
             let minted := mintParams H₀ vs
             EvalRes.withTrace (matchConsume P.decls e k i vs) <|
             (eval M fuel P minted.1
                 { env := minted.2.reverse ++ φ.env, scope := φ.scope ++ minted.2 }
                 body).andThen
               fun H₂ v₂ =>
                 -- (D-EndScope) at the arm's end (`6.3:17`'s timing): the
                 -- payload cells drop-retire **newest-first**, so the last
                 -- component goes first (probe e7). The scrutinee value is
                 -- consumed by the match — its payload lives in these cells now
                 -- — so nothing drops it a second time. An unwinding `return`
                 -- inside the arm never reaches here: it discarded this marker
                 -- with the evaluation context and found the same cells in σ
                 -- instead (§6.9, probe e8).
                 match unwindLocs P.decls H₂ minted.2.reverse with
                 | .error w => .stuck w
                 | .ok (H₃, evs) => .ok H₃ v₂ evs)
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .mkArray T args =>
      -- (D-Array) §6.5: an array literal is a redex once every element is a
      -- value; §6.2's `[ v1, …, v_{i-1}, E, e_{i+1}, … ]` context reduces them
      -- left to right, threading `H`, exactly as a struct literal's
      -- initializers are reduced. `n ≥ 0`, and the empty literal is the
      -- zero-sized `[T; 0]`. There is no declaration to look up and so no
      -- arity check: the value's length **is** the literal's.
      (match evalArgs (fun H' e => eval M fuel P H' φ e) H args with
       | .abort r => r
       | .ok H₁ vs tr => EvalRes.withTrace tr (introVal P.decls H₁ (fun i => .array T i vs)))
  | fuel + 1, P, H, φ, .repeatArray T e n =>
      -- The surface repeat form, whose dynamics is §2's elaboration of it:
      -- `7.1:39` evaluates the operand **exactly once** and copies its result
      -- into each of the `n` slots, which is well defined because `7.1:38`
      -- makes the element type `Copy`. The machine reads that premise off the
      -- value: the elaboration `let t = v; [t, …, t]` would move `t` at its
      -- first use and be stuck at the second when `class(v) ≠ Copy`, so a
      -- non-`Copy` operand refuses with `typeConfusion` rather than duplicating
      -- a value §6 would destroy once (RUE-2400). A checked program never
      -- reaches the refusal: `Typed.repeatArray`'s `T.mult = .copy` premise and
      -- `HasTy.mult_eq` give it (`soundness`).
      (eval M fuel P H φ e).andThen fun H' v =>
        if v.mult P.decls = .copy then introVal P.decls H' (fun i => .array T i (List.replicate n v))
        else .stuck .typeConfusion
  | fuel + 1, P, H, φ, .indexRead p idx πs =>
      -- (D-Index)/(D-Index-Trap) §6.5 at a place below one or more **dynamic**
      -- indices: §6.2's `v[E]` contexts reduce the index expressions left to
      -- right first, then the place is navigated and each index is
      -- bounds-checked "at the moment the path is navigated" (`7.1:10`) —
      -- before anything is read. In range, (D-Use-Untrackable-Dynamic-Copy)
      -- §6.3 hands the leaf on and leaves the array alone. That rule's premise
      -- `class(T) = Copy` is read off the value: §6.3 has no rule for a
      -- non-`Copy` leaf under a dynamic index, so the machine refuses with
      -- `typeConfusion` instead of copying an affine value out of a place it
      -- leaves live (RUE-2400). A checked program never reaches the refusal:
      -- `Typed.indexRead`'s `T.mult = .copy` premise and `HasTy.mult_eq` give
      -- it (`soundness`).
      (match evalArgs (fun H' e => eval M fuel P H' φ e) H idx with
       | .abort r => r
       | .ok H₁ vs tr =>
         EvalRes.withTrace tr <|
           match dynPlace H₁ φ p vs πs with
           | .stuck w => .stuck w
           | .bounds => .panic .bounds []
           | .at _ _ sub ρ =>
             match sub.readAt ρ with
             | .error w => .stuck w
             | .ok leaf =>
               match leaf.toVal with
               | none => .stuck .useAfterMove
               | some v =>
                   if v.mult P.decls = .copy then .ok H₁ v []
                   else .stuck .typeConfusion)
  | fuel + 1, P, H, φ, .indexWrite p idx πs e =>
      -- (D-Assign) §6.8 below a dynamic index, in `5.2:14`'s order: the
      -- right-hand side first, then the index expressions left to right
      -- (§6.2's `assign p = E` and `assign p[ v̄, E, … ] = v`), then the
      -- navigation with the bounds check at every dynamic step (`7.1:10`),
      -- then the overwrite-drop of what the leaf held — its glue on an
      -- affine leaf, nothing on a `Copy` one, and the `linearOverwrite`
      -- monitor where the residue is linear, which the statics' `3.8:77`
      -- premise keeps out of a checked program — and then the store. A trap
      -- or an unwinding `return` in an index abandons the evaluated
      -- right-hand side undropped: a panic runs no drops (§6.12), and the
      -- compiler does the same (probes r08, r14). The `return` case is
      -- RUE-2316's pending-value edge at the right-hand side ("Pending
      -- values" above), affine only because the leaf is never linear.
      (eval M fuel P H φ e).andThen fun H₁ v =>
        match evalArgs (fun H' e' => eval M fuel P H' φ e') H₁ idx with
        | .abort r => r
        | .ok H₂ vs tr =>
          EvalRes.withTrace tr <|
            match dynPlace H₂ φ p vs πs with
            | .stuck w => .stuck w
            | .bounds => .panic .bounds []
            | .at ℓ c sub ρ =>
              match sub.readAt ρ with
              | .error w => .stuck w
              | .ok old =>
                if old.residualLinear P.decls then .stuck .linearOverwrite
                else
                  match dropCell P.decls ℓ old with
                  | .error w => .stuck w
                  | .ok evs =>
                    match sub.writeAt ρ (Contents.ofVal v) with
                    | none => .stuck .typeConfusion
                    | some sub' =>
                      match c.writeAt p.path sub' with
                      | none => .stuck .typeConfusion
                      | some c' =>
                        if c'.copyClosed P.decls then .ok (H₂.set ℓ (.full c')) .unit evs
                        else .stuck .ownedUnderCopy
  | fuel + 1, P, H, φ, .indexDrop p idx πs =>
      -- §6.11's `@drop(p)` at a `Copy` place below a dynamic index. A `Copy`
      -- place owes no glue and changes no ownership, so what is left of the
      -- form is the read's navigation: the indices left to right, then the
      -- bounds check at every dynamic step (`7.1:10`), which traps exactly as
      -- the read does (probe d3). In range, the leaf is left where it is and
      -- the value is `()`. The redex *is* the read with its value discarded,
      -- and it is evaluated as that — so it inherits the read's `Copy` check:
      -- a non-`Copy` leaf refuses with `typeConfusion` rather than stepping to
      -- `()` where §6.11's `@drop` would drop the leaf and write `⊘`
      -- (RUE-2400; the statics make that form E0904).
      (eval M fuel P H φ (.indexRead p idx πs)).andThen fun H' _ => .ok H' .unit []
  | _ + 1, P, H, φ, .drop p =>
      -- §6.11's explicit `@drop(p)`: at a `Declared(d, π_s)` plan it is the
      -- §6.3 destructure with the selected leaf dropped too — residue first,
      -- leaf second (probe d6c) — and the *consumed place* `ℓ@π_d` becomes
      -- `⊘`, whatever the leaf's class (§5.3, probe d6). A `⊘` at the selected
      -- leaf refuses with `useAfterMove`, exactly as the ordinary branch below
      -- refuses a `⊘` at the named place and as the declared `.use` branch
      -- refuses through `toVal`: `Contents.mult ⊘ = .copy`, so without the
      -- guard `dropCell` would report `.ok []` and the redex would complete in
      -- silence. Unreachable for a checked program — `fully-owned(Σ, d)` makes
      -- the whole of `d` hole-free — which is why `soundness`'s `dropDeclared`
      -- case discharges it from `destructure_ok`'s `leaf.holeFree`. Otherwise
      -- it runs the drop of whatever the sub-position holds — the walk skips
      -- every already-`⊘` sub-place — and writes `⊘` back at that position,
      -- which suppresses the later scope-exit drop through it.
      match φ.env[p.root]? with
      | none => .stuck .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => .stuck .unbound
        | some .dead => .stuck .useAfterDrop
        | some (.full c) =>
          match c.declaredPlan P.decls p.path with
          | some (πd, πs) =>
            (match c.readAt πd with
             | .error w => .stuck w
             | .ok cd =>
               match cd.destructure P.decls ℓ πs with
               | .error w => .stuck w
               | .ok (leaf, evs) =>
                 if leaf.isHole then .stuck .useAfterMove else
                 match dropCell P.decls ℓ leaf with
                 | .error w => .stuck w
                 | .ok levs =>
                   match c.writeAt πd .hole with
                   | none => .stuck .typeConfusion
                   | some c' => .ok (H.set ℓ (.full c')) .unit (evs ++ levs))
          | none =>
            match c.readAt p.path with
            | .error w => .stuck w
            | .ok sub =>
              if sub.isHole then .stuck .useAfterMove else
              match dropCell P.decls ℓ sub with
              | .error w => .stuck w
              | .ok evs =>
                  if sub.mult P.decls = .copy then .ok H .unit []
                  else
                    match c.writeAt p.path .hole with
                    | none => .stuck .typeConfusion
                    | some c' => .ok (H.set ℓ (.full c')) .unit evs
  | fuel + 1, P, H, φ, .letIn _m e₁ e₂ =>
      (eval M fuel P H φ e₁).andThen fun H₁ v₁ =>
        -- (D-Let): mint a fresh single-cell binding allocation, bind it, and
        -- register it in the frame's scope record as well as in the
        -- administrative `endscope` the normal path below runs (RUE-1277).
        (eval M fuel P (H₁ ++ [.full (Contents.ofVal v₁)])
            { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂).andThen
          fun H₂ v₂ =>
            -- (D-EndScope): §5.6's obligations, executed. The record this
            -- case returns to is the caller's, which never held the cell, so
            -- the two bookkeepings drop it exactly once between them.
            match dropRetire P.decls H₂ H₁.length with
            | .error w => .stuck w
            | .ok (H₃, evs) => .ok H₃ v₂ evs
  | fuel + 1, P, H, φ, .assign p e =>
      (eval M fuel P H φ e).andThen fun H₁ v =>
        -- (D-Assign) §6.8, at a sub-position: drop what is live there first
        -- (a `⊘` drops nothing — reinitialization, `3.8:55`), then store.
        match φ.env[p.root]? with
        | none => .stuck .unbound
        | some ℓ =>
          match H₁[ℓ]? with
          | none => .stuck .unbound
          | some .dead => .stuck .useAfterDrop
          | some (.full c) =>
            match c.readAt p.path with
            | .error w => .stuck w
            | .ok old =>
                if old.residualLinear P.decls then .stuck .linearOverwrite   -- 3.8:77
                else
                  match dropCell P.decls ℓ old with
                  | .error w => .stuck w
                  | .ok evs =>
                      match c.writeAt p.path (Contents.ofVal v) with
                      | none => .stuck .typeConfusion
                      | some c' =>
                          -- The copy-closure monitor (`Contents.copyClosed`).
                          if c'.copyClosed P.decls then .ok (H₁.set ℓ (.full c')) .unit evs
                          else .stuck .ownedUnderCopy
  | fuel + 1, P, H, φ, .seq e₁ e₂ =>
      (eval M fuel P H φ e₁).andThen fun H₁ v₁ =>
        match v₁.mult P.decls with
        | .linear => .stuck .linearDiscard                         -- 3.8:64
        | .affine =>
            (match dropContents P.decls (Contents.ofVal v₁) with
             | .error w => .stuck w
             | .ok evs => (eval M fuel P H₁ φ e₂).withTrace (.dropTemp v₁ :: evs))
        | .copy => eval M fuel P H₁ φ e₂
  | fuel + 1, P, H, φ, .ite c e₁ e₂ =>
      (eval M fuel P H φ c).andThen fun H₀ v₀ =>
        match v₀ with
        | .bool b => if b then eval M fuel P H₀ φ e₁ else eval M fuel P H₀ φ e₂
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .call f args =>
      match evalArgs (fun H' e => eval M fuel P H' φ e) H args with
      | .abort r => r
      | .ok H₁ vs tr =>
        EvalRes.withTrace tr <|
          match P.fns[f]? with
          | none => .stuck .unbound
          | some fd =>
            if fd.params.length = vs.length then
              -- (D-Call): one fresh cell per by-value argument; the callee's
              -- entry scope owes a drop for exactly those cells.
              let minted := mintParams H₁ vs
              let φg : Frame := { env := minted.2.reverse, scope := minted.2 }
              (eval M fuel P minted.1 φg fd.body).absorb fun H₃ v =>
                -- (D-Return-Value): the body became a value; pop the frame,
                -- running its open scopes' drops. (D-Return) needs no second
                -- path: `absorb` took its value, and its unwind already ran
                -- every one of those drops.
                match runAllScopeDrops P.decls H₃ φg with
                | .error w => .stuck w
                | .ok (H₄, evs) => .ok H₄ v evs
            else .stuck .typeConfusion
  | fuel + 1, P, H, φ, .ret e =>
      (eval M fuel P H φ e).andThen fun H₁ v =>
        -- (D-Return) §6.9: discard the evaluation context, every pending
        -- `endscope` marker inside it included, and run the frame's scope
        -- record instead — newest binding first.
        match runAllScopeDrops P.decls H₁ φ with
        | .error w => .stuck w
        | .ok (H₂, evs) => .returned H₂ v evs
  | fuel + 1, P, H, φ, .loop e =>
      -- §6.10: run the body in the loop's frame. (D-Loop-Iter): when it
      -- becomes a value — `()`, discarded, since the body is `unit`-typed —
      -- its own `let`s and arms have already dropped what they bound (§6.7's
      -- `endscope`), and the loop re-enters its body; each turn spends a unit
      -- of fuel, so an infinite loop exhausts it. (D-Break): a `break` from
      -- the body carries the scope record of the frame it fired in, whose
      -- cells past the loop's own are the body's bindings still open there —
      -- `unwind-drops(H, φ', φ)` drop-retires them, newest-first — and the
      -- whole loop yields `()`. Every other outcome, an unwinding `return`
      -- included, leaves the loop unchanged.
      -- The body's value is `⟨⟩` (§6.10, "necessarily `⟨⟩`, discarded"); any
      -- other value is stuck rather than discarded undropped (RUE-2427), and a
      -- checked program never reaches it: the body is typed `unit`.
      match eval M fuel P H φ e with
      | .ok H₁ .unit tr => (eval M fuel P H₁ φ (.loop e)).withTrace tr
      | .ok _ _ _ => .stuck .typeConfusion
      | .broke H₁ sc tr =>
          match unwindLocs P.decls H₁ (sc.drop φ.scope.length).reverse with
          | .error w => .stuck w
          | .ok (H₂, evs) => .ok H₂ .unit (tr ++ evs)
      | r => r
  | _ + 1, _, H, φ, .brk =>
      -- (D-Break) §6.10: discard the evaluation context — every pending
      -- `endscope` marker inside it included — and hand the loop the frame's
      -- scope record, which is where §6.7 registered every binding the
      -- discarded markers owed a drop (RUE-1277).
      .broke H φ.scope []

/-- A program's outcome (§6.12's top-level result): call the entry function,
index `0`, with no arguments in an empty store and a frame with no bindings.
(D-Return-Main) is the same rule as (D-Return-Value) at the bottom of the
stack, so the entry point is an ordinary call and needs no second path: the
call boundary absorbs an unwinding `return` exactly as it does anywhere. -/
def run (M : FloatOps) (P : Program) (fuel : Nat) : EvalRes :=
  eval M fuel P [] { env := [], scope := [] } (.call 0 [])

end RueCore
