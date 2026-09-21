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
  below, and the carve-out under "Pending arguments" names the one edge they
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
* **The correspondence with §6 is claimed on the checker's input domain.**
  On a program `check` accepts, `eval` and §6 agree (the adequacy lemma
  owed in RUE-2289 is stated there); on other input they may not. An
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
* Arithmetic overflow, a zero divisor, an out-of-range `@intCast` and an
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

## Places, partial moves, and §6.11's drop order

A place is a root binding and a path of field steps (`Place`, `Syntax.lean`),
and §6.3 navigates it into the stored contents: `H(ℓ)@π` is `readAt` and
`H[ℓ@π ↦ ⊘]` is `writeAt`. (D-Use-Move) writes `⊘` at exactly the
sub-position moved — the whole cell for a whole-place use, one field for a
projection, which is the **partial move** of §4.2 — so the later scope-exit
drop of `ℓ` skips it and cannot free it a second time. That skip is
`dropContents`'s `⊘` case, and it is what makes the residual drop of `3.8:73`
path-specific.

A struct's contents is its declaration's index and one contents per field, so
the drop walk is §6.11's own: the user destructor first (`3.9:28`), then the
fields in declaration order (`3.9:13`), recursively, skipping every `⊘`. Every
path that drops — `@drop` (§6.11), scope exit (§6.7), the overwrite (§6.8), a
discarded temporary (§6.7), and the frame teardown (§6.9) — routes through it,
so a trace carries that order wherever a drop happens.
`dropContents_struct_events` (`Soundness.lean`) states the order as a theorem,
in closed form.

Both linear monitors read the **residue** rather than a type. §6.7's and
§6.9's leak monitor refuses when the contents a scope exit reaches still holds
a live declared-`linear` sub-value (`Contents.residualLinear`, §5.6's own
recursion), and §6.8's overwrite monitor refuses on the same reading of the
position being written. A carrier whose linear field has been moved out
therefore drops its remaining residue quietly, which is the RUE-1591 model and
what the compiler does.

## Pending arguments: the one edge no monitor covers

A by-value argument reduces to a value that lives in no cell and in no scope
record between the `use` that produced it and the `mintParams` that gives it
one (§6.9's (D-Call)). If a *later* argument of the same call unwinds by
`return`, (D-Return) §6.9 discards the evaluation context — `g(v̄, …, E, …)`
included — and runs `run-all-scope-drops` on the frame's records, which never
named that value. Its drop is therefore neither run nor monitored, whatever
its multiplicity class: an affine argument emits no `dropTemp`, and a linear
one is destroyed without a `linearDiscard`.

That is the calculus as written, not a modelling slip. (D-Return) unwinds σ
and nothing else, and §5's only bottom rule for an argument position — §5.7's
strict-context rule, `Strict-Bottom` there, which this fragment does not
mechanize because it has no ⊥ provenance to propagate — carries `⊥;δ_e`
outward without imposing §5.3's discard check on the siblings already
evaluated; the statics cannot reject the program without provenance they do
not have. §6.9's own justification for
(D-Return) — "every bound cell is also registered in the frame's scope
records" — is exactly true and exactly insufficient here, because an argument
temporary is not a bound cell. The Rue compiler behaves the same way (a
destructor-bearing argument's destructor does not run), so the bridge cannot
see it either.

`eval` models the calculus rather than patching it, so no monitor is added:
`evalArgs` passes a `returned` abort on untouched. Both shapes are pinned as
kernel-checked witnesses in `Examples.lean`
(`linearLostAtCallArg`, `affineLostAtCallArg`), the §7 claim is stated with
the carve-out named (`Soundness.lean`'s `no_violation`,
`docs/formal/03-metatheory.md`), and closing it is an open spec decision
(RUE-2316, the pending-argument decision) — it needs a rule, in §5.7 or
§6.9, before a monitor here would mean anything.

## Frames, scope records, and unwinding (§6.1, §6.9)

§6.1's frame is `φ = ⟨ρ ; σ⟩` with `σ` a *stack* of open scope records. The
fragment's forms open exactly one scope per frame — `push-scope` belongs to
§6.6's `match` arms and §6.10's loops, neither of which is here, while
(D-Let) appends its cell to the innermost record rather than pushing a new
one — so `Frame` carries that one record. The stack returns with loops.

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

/-- Machine values (§6.1's `v`), fragment forms only. `struct s vs` is §6.1's
`{ v1, …, vk }_S`: the declaration's index and one value per field, in
declaration order — the order `3.9:13` drops them in. `float w f` is §6.1's `f_T` at `T = float(w)`: §2's
datum, not a bit pattern (`Float.lean`). A struct value names its declaration rather than
carrying its class, so the machine's drop decisions are value-driven — it
reads the tag the value carries — while the class and the destructor come from
the program's declarations, as the compiled program's drop glue does. -/
inductive Val where
  | int (w : IntWidth) (s : Sign) (n : Int)
  | float (w : FloatWidth) (f : FloatDatum)
  | bool (b : Bool)
  | unit
  | struct (s : Nat) (fields : List Val)
deriving Repr

/-- The dynamic image of `class(T)` (§3) on a value: scalars are `Copy`, a
struct value has the class its declaration records. -/
def Val.mult (D : StructEnv) : Val → Mult
  | .struct s _ => D.classOf s
  | .int _ _ _ | .float _ _ | .bool _ | .unit => .copy

/-- Cell contents (§6.1's `c ::= v | ⊘`), as a **tree**: `⊘` may sit at any
node, not only at the root, because (D-Use-Move) §6.3 writes `H[ℓ@π ↦ ⊘]` at
exactly the sub-position a partial move takes (§4.2, `3.8:22`). A hole-free
contents is a value (`toVal`), and a value written into a cell becomes the
hole-free tree of the same shape (`ofVal`); the two are inverse, which is what
lets §6.11's walk and §6.3's navigation share one representation. -/
inductive Contents where
  | hole
  | int (w : IntWidth) (s : Sign) (n : Int)
  | float (w : FloatWidth) (f : FloatDatum)
  | bool (b : Bool)
  | unit
  | struct (s : Nat) (cs : List Contents)
deriving Repr

mutual
/-- A value stored into a cell or a sub-position: the same tree with no `⊘` in
it (§6.8's `H[ℓ@π ↦ v]`, §6.7's (D-Let)) (helper). -/
def Contents.ofVal : Val → Contents
  | .int w s n => .int w s n
  | .float w f => .float w f
  | .bool b => .bool b
  | .unit => .unit
  | .struct s vs => .struct s (Contents.ofVals vs)

/-- `ofVal` over a field list (helper). -/
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
  | .struct s cs => (Contents.toVals cs).map (Val.struct s)

/-- `toVal` over a field list (helper). -/
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
  | .int _ _ _ | .float _ _ | .bool _ | .unit | .struct _ _ => false

/-- The dynamic image of `class(T)` (§3) on cell contents: a hole has nothing
to drop, and a struct has the class its declaration records (helper). -/
def Contents.mult (D : StructEnv) : Contents → Mult
  | .struct s _ => D.classOf s
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => .copy

mutual
/-- §5.6's `residual-linear`, read on the **contents** rather than on Σ: does a
live sub-value of a declared-`linear` struct type remain? This is the leak
monitor §6.7's `endscope` and §6.9's frame teardown consult, and the overwrite
monitor of §6.8. A `⊘` carries nothing (`3.8:73`'s skip), a live
declared-`linear` struct carries the obligation itself (`3.8:74`), and
otherwise the obligation is the disjunction over the live fields — exactly the
recursion §5.6 writes for Σ, on the store's side of the invariant. -/
def Contents.residualLinear (D : StructEnv) : Contents → Bool
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => false
  | .struct s cs =>
      (match D[s]? with
       | some sd => sd.attr = .linear || Contents.residualLinearList D cs
       | none => false)

/-- The same over a field list (helper). -/
def Contents.residualLinearList (D : StructEnv) : List Contents → Bool
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
  | .struct s cs, f :: π, new =>
      (match cs[f]? with
       | some c => (Contents.writeAt c π new).map fun c' => .struct s (cs.set f c')
       | none => none)
  | _, _ :: _, _ => none

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
it rather than a value; a discarded temporary is always a whole value. -/
inductive Event where
  | drop (ℓ : Nat) (c : Contents)
  | dropTemp (v : Val)
  | dtor (s : Nat) (c : Contents)
  | dbg (v : Val)
deriving Repr

/-- Defined traps (§6.12's `↯κ`), the categories the fragment reaches.
`bounds` is the arrays', which are not here; `rem-zero` and `user` are §6.12's
own spellings, and `cast-overflow` is the one §6.12 gains with `@intCast`
(`4.13:28`) — the implementations report it as `integer cast overflow`,
distinct from the arithmetic overflow, so the model keeps them apart. -/
inductive PanicKind where
  | overflow
  | divZero
  | remZero
  | castOverflow
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
  reaching a live linear value (§7: consumed exactly once; §5.6). -/
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
deriving DecidableEq, Repr

/-- `H(ℓ)@π` (§6.3): follow a path into the stored contents. Reaching a `⊘`
with path left to walk is the use of a moved-out place (§7's first bullet); a
step that is not a field of what is stored is a shape no well-typed program
produces (helper). -/
def Contents.readAt : Contents → List Nat → Except Violation Contents
  | c, [] => .ok c
  | .hole, _ :: _ => .error .useAfterMove
  | .struct _ cs, f :: π =>
      (match cs[f]? with
       | some c => Contents.readAt c π
       | none => .error .typeConfusion)
  | _, _ :: _ => .error .typeConfusion

/-- Evaluation results: a value with the final store and trace (§6.12's normal
result); a value handed back by an unwinding `return`, whose frame's scopes
have already been dropped (§6.9's (D-Return)) and which every enclosing form
passes on untouched until a call boundary absorbs it; a defined panic
(§6.12's `↯κ`); a violation ("stuck": either a configuration §6 leaves
undefined or a linear action one of the monitors refuses, named; the module
docstring says which is which); or exhausted fuel, which is not a machine
state at all but this interpreter's admission that it stopped early. -/
inductive EvalRes where
  | ok (H : Store) (v : Val) (tr : List Event)
  | returned (H : Store) (v : Val) (tr : List Event)
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
  | .panic k tr' => .panic k (tr ++ tr')
  | r => r

/-- §6.2's evaluation-context search, as a combinator: run an operand, and if
it reduced to a value, continue in the context with the store it left, the
operand's trace prefixed onto whatever the context produces. Every other
outcome — a trap (§6.12), a refusal, exhausted fuel, and an unwinding `return`
(§6.9's (D-Return), which discards the context `E` it is under) — is the whole
form's outcome, unchanged. -/
def EvalRes.andThen : EvalRes → (Store → Val → EvalRes) → EvalRes
  | .ok H v tr, k => (k H v).withTrace tr
  | r, _ => r

/-- §6.9's call boundary, as a combinator: the same search as `andThen`,
except that an unwinding `return` stops here. (D-Return) hands its value to
the suspended caller context, so at the one form that suspended a caller — a
call — a `returned` result becomes the call's value, with the drops its unwind
already ran. Everywhere else the `return` keeps travelling (`andThen`). -/
def EvalRes.absorb : EvalRes → (Store → Val → EvalRes) → EvalRes
  | .ok H v tr, k => (k H v).withTrace tr
  | .returned H v tr, _ => .ok H v tr
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

One thing §6.11 does **not** write out and this walk must: the destructor case
is stated over a *value* `{v1,…,vk}_S`, so the calculus says nothing about a
destructor-bearing struct one of whose fields is `⊘`. `3.9:34` is exactly what
makes that state unreachable — no partial move may be taken under a
destructor-bearing value — and the walk therefore runs the destructor on
whatever the cell holds, hole or not, rather than refusing a state no rule
excludes. `Soundness.lean` proves the state is never reached. -/
def dropContents (D : StructEnv) : Contents → Except Violation (List Event)
  | .hole => .ok []
  | .int _ _ _ => .ok []
  | .float _ _ => .ok []
  | .bool _ => .ok []
  | .unit => .ok []
  | .struct s cs =>
      match D[s]? with
      | none => .error .unbound
      | some sd =>
          match dropContentsList D cs with
          | .error w => .error w
          | .ok evs =>
              .ok ((if sd.dtor then [Event.dtor s (.struct s cs)] else []) ++ evs)

/-- `drop*(H, [c1,…,ck])` (§6.11): fold `drop` over the contents left to right
— for a struct's fields, declaration order (`3.9:13`). -/
def dropContentsList (D : StructEnv) : List Contents → Except Violation (List Event)
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
recursively, every `⊘` skipped. An index the environment does not have emits
nothing, which the walk itself refuses instead —
`dropContents_struct_events` (`Soundness.lean`) is the theorem that the two
agree on every well-typed contents, and it is the closed form RUE-2237's
"dropped exactly once" quantifies over. -/
def dropEvents (D : StructEnv) : Contents → List Event
  | .hole => []
  | .int _ _ _ => []
  | .float _ _ => []
  | .bool _ => []
  | .unit => []
  | .struct s cs =>
      (match D[s]? with
       | some sd => if sd.dtor then [Event.dtor s (.struct s cs)] else []
       | none => []) ++ dropEventsList D cs

/-- The same over a field list: the fields' events concatenated in
declaration order (`3.9:13`), which is §6.11's `drop*`. -/
def dropEventsList (D : StructEnv) : List Contents → List Event
  | [] => []
  | c :: cs => dropEvents D c ++ dropEventsList D cs
end

/-- A field list's events are its fields' events concatenated, left to right:
the flattening `dropValue_struct_events` states the order with (helper). -/
theorem dropEventsList_eq_flatten (D : StructEnv) :
    ∀ cs : List Contents, dropEventsList D cs = (cs.map (dropEvents D)).flatten
  | [] => rfl
  | c :: cs => by simp [dropEventsList, dropEventsList_eq_flatten D cs]

/-- The drop of a binding cell's contents (§6.11), as the trace records it: a
`drop ℓ c` marker naming the cell, then the events the contents' own drop
emits. `Copy` contents — a scalar, or a `⊘` with nothing left in it — has no
drop glue at all (§6.11: `drop(H, n_T) = H`, `drop(H, ⊘) = H`), so it records
nothing, which is also why `@drop` of a `Copy` place leaves no trace (§5.3's
(@Drop-Copy)) (helper). -/
def dropCell (D : StructEnv) (ℓ : Nat) (c : Contents) : Except Violation (List Event) :=
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
def dropRetire (D : StructEnv) (H : Store) (ℓ : Nat) : Except Violation (Store × List Event) :=
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
def unwindLocs (D : StructEnv) (H : Store) : List Nat → Except Violation (Store × List Event)
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
def runAllScopeDrops (D : StructEnv) (H : Store) (φ : Frame) :
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

/-- The interpreter, over a `FloatOps` (`Float.lean`): §2 fixes `rnd_w` and
`σ_NaN` per *target*, not per rule, so the machine takes them as a parameter
and every theorem quantifies over a model that satisfies §7's laws. Rule
correspondence, per case: `use` is
(D-Use-Copy)/(D-Use-Move) (§6.3); `binop`, `unop`, `intCast` and
`fintrin` are §6.4's operator and intrinsic rules, computed by
`evalBinOp`/`evalUnOp`/`evalIntCast`/`evalFintrin` above, the float half of
them through the model `M`;
`panic` is (D-Panic) §6.12; `dbg` appends the operand's rendering to the
observable output (§5.8's (Dbg), §6.12's `Outcome`); `drop` is §6.11's
explicit `@drop`; `letIn` is (D-Let) + (D-EndScope)'s drop-retire (§6.7);
`assign` is (D-Assign), §6.8's overwrite-drop / reinitialization; `seq` is
(D-Seq), discarding with a temporary drop (§6.7); `mkStruct` is (D-Struct)
§6.5 after §6.2's left-to-right search through its initializers; `ite` is
(D-If-T)/(D-If-F) after the §6.2 search for the scrutinee; `call` is (D-Call)
followed by (D-Return-Value) when the body completes normally, and by
(D-Return)'s absorption when it does not; `ret` is (D-Return), which runs the
frame's scope drops and hands the value past every enclosing form.

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
      -- (D-Use-Copy)/(D-Use-Move) §6.3: resolve the root under ρ, navigate the
      -- path into the stored aggregate, and — for a non-`Copy` place — write
      -- `⊘` at exactly that sub-position, which is the partial move of §4.2.
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
            match sub.toVal with
            | none => .stuck .useAfterMove
            | some v =>
                if v.mult P.structs = .copy then .ok H v []
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
      (eval M fuel P H φ e).andThen fun H' v => .ok H' .unit [.dbg v]
  | fuel + 1, P, H, φ, .mkStruct s args =>
      -- (D-Struct) §6.5: a struct literal is a redex once every initializer
      -- is a value; §6.2's contexts reduce them left to right, threading `H`,
      -- exactly as a call's arguments are reduced (§6.9).
      (match evalArgs (fun H' e => eval M fuel P H' φ e) H args with
       | .abort r => r
       | .ok H₁ vs tr =>
         EvalRes.withTrace tr <|
           match P.structs[s]? with
           | none => .stuck .unbound
           | some sd =>
               if sd.fields.length = vs.length then .ok H₁ (.struct s vs) []
               else .stuck .typeConfusion)
  | _ + 1, P, H, φ, .drop p =>
      -- §6.11's explicit `@drop(p)`: run the drop of whatever the
      -- sub-position holds — the walk skips every already-`⊘` sub-place — and
      -- write `⊘` back at that position, which suppresses the later
      -- scope-exit drop through it.
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
            if sub.isHole then .stuck .useAfterMove else
            match dropCell P.structs ℓ sub with
            | .error w => .stuck w
            | .ok evs =>
                if sub.mult P.structs = .copy then .ok H .unit []
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
            match dropRetire P.structs H₂ H₁.length with
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
                if old.residualLinear P.structs then .stuck .linearOverwrite   -- 3.8:77
                else
                  match dropCell P.structs ℓ old with
                  | .error w => .stuck w
                  | .ok evs =>
                      match c.writeAt p.path (Contents.ofVal v) with
                      | none => .stuck .typeConfusion
                      | some c' => .ok (H₁.set ℓ (.full c')) .unit evs
  | fuel + 1, P, H, φ, .seq e₁ e₂ =>
      (eval M fuel P H φ e₁).andThen fun H₁ v₁ =>
        match v₁.mult P.structs with
        | .linear => .stuck .linearDiscard                         -- 3.8:64
        | .affine =>
            (match dropContents P.structs (Contents.ofVal v₁) with
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
                match runAllScopeDrops P.structs H₃ φg with
                | .error w => .stuck w
                | .ok (H₄, evs) => .ok H₄ v evs
            else .stuck .typeConfusion
  | fuel + 1, P, H, φ, .ret e =>
      (eval M fuel P H φ e).andThen fun H₁ v =>
        -- (D-Return) §6.9: discard the evaluation context, every pending
        -- `endscope` marker inside it included, and run the frame's scope
        -- record instead — newest binding first.
        match runAllScopeDrops P.structs H₁ φ with
        | .error w => .stuck w
        | .ok (H₂, evs) => .returned H₂ v evs

/-- A program's outcome (§6.12's top-level result): call the entry function,
index `0`, with no arguments in an empty store and a frame with no bindings.
(D-Return-Main) is the same rule as (D-Return-Value) at the bottom of the
stack, so the entry point is an ordinary call and needs no second path: the
call boundary absorbs an unwinding `return` exactly as it does anywhere. -/
def run (M : FloatOps) (P : Program) (fuel : Nat) : EvalRes :=
  eval M fuel P [] { env := [], scope := [] } (.call 0 [])

end RueCore
