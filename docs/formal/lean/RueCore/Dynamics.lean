import RueCore.Statics

/-!
# RueCore.Dynamics — the executable machine (§6)

A definitional interpreter over the §6.1 configuration shape, restricted to
the fragment: a store of single-cell binding allocations (`full v` / the
moved-out marker `⊘` = `moved` / the retired marker `†` = `dead`), a frame
holding the environment `ρ` and the scope record `σ`, and a drop trace — the
fragment's image of the oracle's observable `Outcome` (drop trace + result).

Design commitments carried over from §6:

* **Memory violations are refusals, not silence.** Reading a `⊘`/`†` cell,
  implicitly dropping a linear value, or overwriting one, yields a named
  `Violation`. The §7 safety theorem (`Soundness.lean`) is exactly:
  well-typed programs never reach one. The refusals are of two kinds, and
  the distinction matters for what `eval` is a model of:
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
  owed in RUE-2289 is stated there); on other input they may not, and not
  only through the monitors. `eval` inspects an operand's shape before
  evaluating the next operand, where §6.2's `v ⊕ E` context reduces the
  next operand first: `add (boolLit true) (div 1 0)` is `typeConfusion`
  here and a division-by-zero panic under §6 (`Examples.lean` pins this).
  A raw `intLit` outside `int(64, signed)` evaluates to its value here,
  while §6's integer domain is bounded and `check` rejects the literal.
* Scope exit *retires* the binding's allocation (`drop-retire`, §6.1), so a
  use after scope exit is `useAfterDrop`, distinct from `useAfterMove`.
* `@drop` and overwrite-drop do **not** retire (§6.8/§6.11): the binding
  stays reinitializable.
* Arithmetic overflow and division by zero are *panics* (`↯` in §6.12), a
  defined outcome permitted by the safety theorem, not a violation.
* Traces record each drop (`drop ℓ v`) and each discarded temporary
  (`dropTemp v`) — the §6.7 temporary-death analog.

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
instead, so every live binding of the frame is still dropped, newest-first
(§6.9's (D-Return); `3.9:18`).

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

/-- Machine values (§6.1's `v`), fragment forms only. A resource value carries
its class so the machine's drop decisions are value-driven, as the oracle's
are (the compiled program's drop glue is type-driven; the §7 preservation
invariant is exactly why the two agree). -/
inductive Val where
  | int (n : Int)
  | bool (b : Bool)
  | unit
  | res (κ : Mult) (n : Int)
deriving DecidableEq, Repr

/-- The dynamic image of `class(T)` (§3) on a value. -/
def Val.mult : Val → Mult
  | .res κ _ => κ
  | _ => .copy

/-- Cell contents (§6.1's `c ::= v | ⊘`, plus the retired allocation `†`). -/
inductive Cell where
  | full (v : Val)
  | moved
  | dead
deriving DecidableEq, Repr

/-- The store `H` (§6.1): locations are indices; allocation appends. A dead
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

/-- Observable drop events, the fragment's slice of the oracle `Outcome`: a
binding's drop (§6.11: at scope exit §6.7, at an unwinding exit §6.9, at
`@drop`, or at an overwrite §6.8) and a discarded temporary's drop ((D-Seq),
§6.7). -/
inductive Event where
  | drop (ℓ : Nat) (v : Val)
  | dropTemp (v : Val)
deriving DecidableEq, Repr

/-- Defined traps (§6.12's `↯κ`), fragment categories. -/
inductive PanicKind where
  | overflow
  | divZero
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
  | panic (k : PanicKind)
  | stuck (why : Violation)
  | outOfFuel
deriving Repr

/-- Prefix a trace onto a result's trace. A `returned` result carries the
drops that ran before and during its unwind, so it takes the prefix exactly as
a normal result does (helper). -/
def EvalRes.withTrace (tr : List Event) : EvalRes → EvalRes
  | .ok H v tr' => .ok H v (tr ++ tr')
  | .returned H v tr' => .returned H v (tr ++ tr')
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

/-- `drop-retire(H, ℓ)` (§6.1): run the binding's drop (§6.11 — a no-op on a
`⊘` or `Copy` cell), then retire the allocation, so any later access to it is
`useAfterDrop` rather than silently readable (the RUE-390 change). A live
linear value here is §5.6's leak: the scope ends with an obligation
undischarged, and the machine refuses (`3.8:32`). This is the one
scope-teardown path: `let`'s normal `endscope` (§6.7) and the frame unwind of
`return` (§6.9) both run it. -/
def dropRetire (H : Store) (ℓ : Nat) : Except Violation (Store × List Event) :=
  match H[ℓ]? with
  | none => .error .unbound
  | some .dead => .error .useAfterDrop
  | some .moved => .ok (H.set ℓ .dead, [])
  | some (.full v) =>
      match v.mult with
      | .linear => .error .linearLeak
      | .affine => .ok (H.set ℓ .dead, [.drop ℓ v])
      | .copy => .ok (H.set ℓ .dead, [])

/-- `run-scope-drops` (§6.1): drop-retire a scope's cells in the order given,
accumulating the drop events. Callers pass the record newest-first, which is
the order §6.1 fixes for a scope's teardown (RAII). -/
def unwindLocs (H : Store) : List Nat → Except Violation (Store × List Event)
  | [] => .ok (H, [])
  | ℓ :: rest =>
      match dropRetire H ℓ with
      | .error w => .error w
      | .ok (H₁, evs) =>
          match unwindLocs H₁ rest with
          | .error w => .error w
          | .ok (H₂, evs') => .ok (H₂, evs ++ evs')

/-- `run-all-scope-drops(H, φ)` (§6.1, §6.9): the whole-frame teardown, run
when a frame is popped — at a normal (D-Return-Value) and at an unwinding
(D-Return). The frame's record lists its cells in creation order, so the
teardown reads it backwards: newest binding first. -/
def runAllScopeDrops (H : Store) (φ : Frame) : Except Violation (Store × List Event) :=
  unwindLocs H φ.scope.reverse

/-- (D-Call) §6.9: mint one fresh single-cell binding allocation per by-value
argument, left to right, each holding its argument's value. Returns the store
and the locations in creation order — the callee's entry scope record, which
owes a drop for exactly these cells. The callee's environment is its reverse,
because `Env` (like `Ctx`) lists the innermost binder first and the last
parameter is the innermost. -/
def mintParams : Store → List Val → Store × List Nat
  | H, [] => (H, [])
  | H, v :: vs =>
      let (H', locs) := mintParams (H ++ [.full v]) vs
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

/-- The interpreter. Rule correspondence, per case: `use` is
(D-Use-Copy)/(D-Use-Move) (§6.3); `add` is (D-Arith)/(D-Arith-Trap), `div` is
(D-Div)/(D-Div-Zero)/(D-Div-Overflow), and `lt` is §6.4's ordering compare
(`cmp`, unlabeled there); `drop` is §6.11's explicit `@drop`; `letIn` is
(D-Let) + (D-EndScope)'s drop-retire (§6.7); `assign` is (D-Assign), §6.8's
overwrite-drop / reinitialization; `seq` is (D-Seq), discarding with a
temporary drop (§6.7); `ite` is (D-If-T)/(D-If-F) after the §6.2 search for
the scrutinee; `call` is (D-Call) followed by (D-Return-Value) when the body
completes normally, and by (D-Return)'s absorption when it does not; `ret` is
(D-Return), which runs the frame's scope drops and hands the value past every
enclosing form.

Every operand is sequenced with `andThen`, which is §6.2's search through an
evaluation context; the callee's body is sequenced with `absorb`, the one
place a `return` stops travelling (§6.9). -/
def eval : Nat → Program → Store → Frame → Expr → EvalRes
  | 0, _, _, _, _ => .outOfFuel
  | _ + 1, _, H, _, .intLit n => .ok H (.int n) []
  | _ + 1, _, H, _, .boolLit b => .ok H (.bool b) []
  | _ + 1, _, H, _, .unitLit => .ok H .unit []
  | _ + 1, _, H, φ, .use i =>
      match φ.env[i]? with
      | none => .stuck .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => .stuck .unbound
        | some .dead => .stuck .useAfterDrop
        | some .moved => .stuck .useAfterMove
        | some (.full v) =>
            if v.mult = .copy then .ok H v []
            else .ok (H.set ℓ .moved) v []
  | fuel + 1, P, H, φ, .add e₁ e₂ =>
      (eval fuel P H φ e₁).andThen fun H₁ v₁ =>
        match v₁ with
        | .int n₁ =>
            (eval fuel P H₁ φ e₂).andThen fun H₂ v₂ =>
              match v₂ with
              | .int n₂ =>
                  if InBounds (n₁ + n₂) then .ok H₂ (.int (n₁ + n₂)) []
                  else .panic .overflow
              | _ => .stuck .typeConfusion
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .div e₁ e₂ =>
      (eval fuel P H φ e₁).andThen fun H₁ v₁ =>
        match v₁ with
        | .int n₁ =>
            (eval fuel P H₁ φ e₂).andThen fun H₂ v₂ =>
              match v₂ with
              | .int n₂ =>
                  if n₂ = 0 then .panic .divZero
                  else if InBounds (n₁.tdiv n₂) then .ok H₂ (.int (n₁.tdiv n₂)) []
                  else .panic .overflow
              | _ => .stuck .typeConfusion
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .lt e₁ e₂ =>
      (eval fuel P H φ e₁).andThen fun H₁ v₁ =>
        match v₁ with
        | .int n₁ =>
            (eval fuel P H₁ φ e₂).andThen fun H₂ v₂ =>
              match v₂ with
              | .int n₂ => .ok H₂ (.bool (decide (n₁ < n₂))) []
              | _ => .stuck .typeConfusion
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .mkres κ e =>
      (eval fuel P H φ e).andThen fun H' v =>
        match v with
        | .int n => .ok H' (.res κ n) []
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .consume e =>
      (eval fuel P H φ e).andThen fun H' v =>
        match v with
        | .res _ n => .ok H' (.int n) []
        | _ => .stuck .typeConfusion
  | _ + 1, _, H, φ, .drop i =>
      match φ.env[i]? with
      | none => .stuck .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => .stuck .unbound
        | some .dead => .stuck .useAfterDrop
        | some .moved => .stuck .useAfterMove
        | some (.full v) =>
            if v.mult = .copy then .ok H .unit []
            else .ok (H.set ℓ .moved) .unit [.drop ℓ v]
  | fuel + 1, P, H, φ, .letIn _m e₁ e₂ =>
      (eval fuel P H φ e₁).andThen fun H₁ v₁ =>
        -- (D-Let): mint a fresh single-cell binding allocation, bind it, and
        -- register it in the frame's scope record as well as in the
        -- administrative `endscope` the normal path below runs (RUE-1277).
        (eval fuel P (H₁ ++ [.full v₁])
            { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂).andThen
          fun H₂ v₂ =>
            -- (D-EndScope): §5.6's obligations, executed. The record this
            -- case returns to is the caller's, which never held the cell, so
            -- the two bookkeepings drop it exactly once between them.
            match dropRetire H₂ H₁.length with
            | .error w => .stuck w
            | .ok (H₃, evs) => .ok H₃ v₂ evs
  | fuel + 1, P, H, φ, .assign i e =>
      (eval fuel P H φ e).andThen fun H₁ v =>
        match φ.env[i]? with
        | none => .stuck .unbound
        | some ℓ =>
          match H₁[ℓ]? with
          | none => .stuck .unbound
          | some .dead => .stuck .useAfterDrop
          | some .moved => .ok (H₁.set ℓ (.full v)) .unit []      -- reinit (3.8:55)
          | some (.full vOld) =>
              match vOld.mult with
              | .linear => .stuck .linearOverwrite                 -- 3.8:77
              | .affine => .ok (H₁.set ℓ (.full v)) .unit [.drop ℓ vOld]
              | .copy => .ok (H₁.set ℓ (.full v)) .unit []
  | fuel + 1, P, H, φ, .seq e₁ e₂ =>
      (eval fuel P H φ e₁).andThen fun H₁ v₁ =>
        match v₁.mult with
        | .linear => .stuck .linearDiscard                         -- 3.8:64
        | .affine => (eval fuel P H₁ φ e₂).withTrace [.dropTemp v₁]
        | .copy => eval fuel P H₁ φ e₂
  | fuel + 1, P, H, φ, .ite c e₁ e₂ =>
      (eval fuel P H φ c).andThen fun H₀ v₀ =>
        match v₀ with
        | .bool b => if b then eval fuel P H₀ φ e₁ else eval fuel P H₀ φ e₂
        | _ => .stuck .typeConfusion
  | fuel + 1, P, H, φ, .call f args =>
      match evalArgs (fun H' e => eval fuel P H' φ e) H args with
      | .abort r => r
      | .ok H₁ vs tr =>
        EvalRes.withTrace tr <|
          match P[f]? with
          | none => .stuck .unbound
          | some fd =>
            if fd.params.length = vs.length then
              -- (D-Call): one fresh cell per by-value argument; the callee's
              -- entry scope owes a drop for exactly those cells.
              let minted := mintParams H₁ vs
              let φg : Frame := { env := minted.2.reverse, scope := minted.2 }
              (eval fuel P minted.1 φg fd.body).absorb fun H₃ v =>
                -- (D-Return-Value): the body became a value; pop the frame,
                -- running its open scopes' drops. (D-Return) needs no second
                -- path: `absorb` took its value, and its unwind already ran
                -- every one of those drops.
                match runAllScopeDrops H₃ φg with
                | .error w => .stuck w
                | .ok (H₄, evs) => .ok H₄ v evs
            else .stuck .typeConfusion
  | fuel + 1, P, H, φ, .ret e =>
      (eval fuel P H φ e).andThen fun H₁ v =>
        -- (D-Return) §6.9: discard the evaluation context, every pending
        -- `endscope` marker inside it included, and run the frame's scope
        -- record instead — newest binding first.
        match runAllScopeDrops H₁ φ with
        | .error w => .stuck w
        | .ok (H₂, evs) => .returned H₂ v evs

/-- A program's outcome (§6.12's top-level result): call the entry function,
index `0`, with no arguments in an empty store and a frame with no bindings.
(D-Return-Main) is the same rule as (D-Return-Value) at the bottom of the
stack, so the entry point is an ordinary call and needs no second path: the
call boundary absorbs an unwinding `return` exactly as it does anywhere. -/
def run (P : Program) (fuel : Nat) : EvalRes :=
  eval fuel P [] { env := [], scope := [] } (.call 0 [])

end RueCore
