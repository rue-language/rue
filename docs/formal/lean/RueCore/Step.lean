import RueCore.Dynamics

/-!
# RueCore.Step — §6's reduction relation, over the §6.1 configuration

`Dynamics.lean` presents §6 as a function: `eval` runs an expression to its
end. This module presents it as §6 itself does, as a small-step relation
`Step M P C C'` between machine configurations, one constructor per rule. The
two presentations are of one dynamics; the adequacy theorems that say so are
RUE-2289's parts 2 and 3. This part defines the relation and proves only what
is cheap: it is deterministic, a terminal configuration takes no step, and
its stuck configurations are exactly the ones §6 leaves undefined.

## The configuration (§6.1)

§6.1 writes `C ::= ⟨H ; φ ; K ; e⟩ | ↯κ | ✓n`, with the control stack
`K ::= halt | ret(E, φ)·K | loopβ(e, φ)·K` and the evaluation context `E` of
§6.2 inside `e`. Here:

* `H` and `φ` are the interpreter's own `Store` and `Frame`, so the adequacy
  proofs relate the two presentations without translating a store.
* `K` and the `E` inside `e` are **one list of frames** (`Kont`): each frame
  is one production of §6.2's `E` grammar with its hole on top, and the
  paper's `ret(E, φ)` and `loopβ(e, φ)` are frames too. The paper's
  `⟨H ; φ ; K ; E[e]⟩` is `run H φ (frames(E) ++ K♭) (eval e)`, where `K♭`
  writes `ret(E', φ')·K` as `call φ' :: frames(E') ++ K♭`. This is the usual
  refocusing of a context grammar into a stack machine; it needs no runtime
  term syntax, so `Expr` (`Syntax.lean`) is unchanged.
* The focus (`Focus`) says what the top of the configuration is doing:
  evaluating an expression, returning a value into the top frame's hole, or
  moving along an argument list.
* §6.12's observable output — the `@dbg` lines and the drop trace — is
  carried in the configuration, because §6.12 says it "accumulates during
  reduction"; a trap keeps it (`Config.panic`). `✓n` is a `run` whose stack
  is empty and whose focus is a value (`Config.Terminal`), from which
  (Result-Ok) reads `n`.

## Search rules as constructors (§6.2)

§6.2 drives reduction with two structural rules, (Search) and (Panic-Lift).
Here (Search) is not a single congruence constructor: every `E` production
contributes an *enter* rule (the focus moves into the hole and the frame is
pushed) and a *plug* rule (a value returns into the hole: move to the next
hole, or fire the redex). Decomposition of a configuration into a context and
a redex is then unique by construction, which is what makes determinism a
case split (`Step.det`). (Panic-Lift) is folded into every trap rule: the
result `Config.panic κ tr` drops the whole stack, which is "a panic is not a
value and no context can consume it".

## What this relation does not check (RUE-2314)

`eval` adds three **monitors** §6 does not have: `linearLeak` at a scope exit
or a destructure's residue, `linearOverwrite` at an assignment, and
`linearDiscard` at a sequence. §6.7, §6.8 and §6.3 execute those drops and
rely on §5 to have excluded the linear case, so `Step` executes them too:
`plainDropRetire`, `plainUnwind` and `plainDestructure` are the interpreter's
helpers with the monitor removed, and (D-Assign) and (D-Seq) below carry no
linearity premise. `step_stuck_inSix` proves the relation's stuck states are
named by §6's own four violations only; `unwindLocs_plain` and
`destructure_plain` prove that where a monitor lets a drop through, the plain
drop does the same thing.

## Two deviations in form

* **The environment is restored at `endscope`.** §6.7 never restores `ρ`
  after a binding dies, because elaboration α-renames binders and a dead name
  is never looked up. Bindings here are de Bruijn indices, and a stale index
  *would* be looked up by the continuation, so the `endscope` frame carries
  the frame to resume, exactly as `eval` resumes its caller's. On every
  reachable configuration the resumed frame is the paper's `⟨ρ; s minus ℓ̄⟩`.
* **The loop boundary keeps its context** (a calculus finding, recorded in
  RUE-2324's report). §6.10's `loopβ(e, φ)` records no context, so under
  (Search) the rules as written would, at `E[loop { e }]`, hand the body's
  first value to `E` and let `break` discard `E` with `E'`. (D-Call) states
  its context explicitly (`ret(E, φ)`); the loop frame here sits above `E`'s
  frames the same way, so the loop yields `()` to its context, as the
  compiler and `eval` do. §6.10's `push-scope` is `eval`'s single scope
  record read by length, as in `Dynamics.lean`.
-/

namespace RueCore

/-! ## Frames, focus, configurations -/

/-- The redex an argument list completes into: the list-shaped contexts of
§6.2 — `S{ v̄, E, ē }`, `Kj( v̄, E, ē )`, `[ v̄, E, ē ]`, `g( v̄, E, ē )` and a
dynamic place's indices `p[ v̄, E, ē ]` — share one frame, and this tag says
which form the list belongs to. `indexWrite` carries the right-hand side's
value, which §6.2's `assign p = E` reduced before the indices (`5.2:14`). -/
inductive ArgsTag where
  | struct (s : Nat)
  | enum (e : Nat) (k : Nat)
  | array (elem : Ty)
  | call (f : Nat)
  | indexRead (p : Place) (πs : List (List Nat))
  | indexDrop (p : Place) (πs : List (List Nat))
  | indexWrite (p : Place) (πs : List (List Nat)) (v : Val)
deriving Repr

/-- One frame of §6.1's control stack `K`, with §6.2's evaluation contexts `E`
flattened onto it (the module docstring says how the two correspond). Every
production of §6.2's `E` grammar the fragment has is a frame whose hole is the
top of the stack; `endscope` is §6.7's administrative form; `loop` is
§6.10's `loopβ(e, φ)`; `call` is §6.9's `ret(E, φ)`, with `E` the frames
below it. -/
inductive Kont where
  /-- `E ⊕ e` (§6.2): the left operand is being reduced. -/
  | binopL (op : BinOp) (e₂ : Expr)
  /-- `v ⊕ E` (§6.2): the right operand is being reduced. -/
  | binopR (op : BinOp) (v₁ : Val)
  /-- `⊖ E` (§6.2). -/
  | unop (op : UnOp)
  /-- `@intCast( E )`, an intrinsic operand (§6.2). -/
  | intCast (w : IntWidth) (s : Sign)
  /-- `@f( E )`, a float intrinsic's operand (§6.2). -/
  | fintrin (k : FloatIntrin)
  /-- `@dbg( E )` (§6.2). -/
  | dbg
  /-- An argument list's next hole, `…( v̄, E, ē )` (§6.2): the values
  already reduced and the expressions still to come. -/
  | args (t : ArgsTag) (vs : List Val) (es : List Expr)
  /-- `[ E; n ]`, the surface repeat form's operand (`7.1:39`). -/
  | repeatArray (elem : Ty) (n : Nat)
  /-- `assign p[ ē ] = E` (§6.2): the right-hand side first (`5.2:14`). -/
  | indexWriteRhs (p : Place) (idx : List Expr) (πs : List (List Nat))
  /-- `match E { … }` (§6.2). -/
  | «match» (arms : List Expr)
  /-- `let x = E ; e2` (§6.2). -/
  | letIn (e₂ : Expr)
  /-- `E ; e2` (§6.2). -/
  | seq (e₂ : Expr)
  /-- `if E { e1 } else { e2 }` (§6.2). -/
  | ite (e₁ e₂ : Expr)
  /-- `assign p = E` (§6.2). -/
  | assign (p : Place)
  /-- `return E` (§6.2). -/
  | ret
  /-- `endscope(ℓ̄) in E` (§6.2, §6.7), with the frame to resume when it
  closes (the module docstring says why the frame is carried). -/
  | endscope (ℓs : List Nat) (φ : Frame)
  /-- `loopβ(e, φ)` (§6.1, §6.10): the loop's body and the frame each turn
  starts in; `break` unwinds to here. -/
  | loop (e : Expr) (φ : Frame)
  /-- `ret(E, φ)` (§6.1, §6.9): a caller suspended in frame `φ`, its context
  `E` the frames below this one. -/
  | call (φ : Frame)
deriving Repr

/-- What the top of a configuration is doing: reducing an expression, handing
a value to the hole of the top frame (§6.2's `E[v]`), or moving along an
argument list towards its next hole or its redex (§6.2's list contexts). -/
inductive Focus where
  | eval (e : Expr)
  | ret (v : Val)
  | args (t : ArgsTag) (vs : List Val) (es : List Expr)
deriving Repr

/-- §6.1's machine configuration. `run H φ K f tr` is `⟨H ; φ ; K ; e⟩`, with
the observable output `tr` so far (§6.12); `panic κ tr` is `↯κ`, keeping
the output the trapping program had produced. `✓n` is not a separate
constructor: it is a `run` with an empty stack and a value in focus
(`Config.Terminal`), from which (Result-Ok) reads the exit code. -/
inductive Config where
  | run (H : Store) (φ : Frame) (K : List Kont) (f : Focus) (tr : List Event)
  | panic (k : PanicKind) (tr : List Event)
deriving Repr

/-- §6.12's initial configuration: the empty store (the fragment has no
string literals to pre-allocate), an empty frame, and the entry point called
with no arguments — the same call `run` (`Dynamics.lean`) makes, so
(D-Return-Main) is (D-Return-Value) at the bottom of the stack. -/
def Config.init : Config :=
  .run [] { env := [], scope := [] } [] (.eval (.call 0 [])) []

/-- The terminal configurations: `✓n`, a value with nothing left to plug it
into — (Result-Ok) — and `↯κ` — (Result-Panic). -/
def Config.Terminal : Config → Prop
  | .run _ _ [] (.ret _) _ => True
  | .panic _ _ => True
  | .run _ _ _ _ _ => False

/-! ## Stack searches and monitor-free drops -/

/-- The nearest `ret(E, φ)` below the top: its frame and the stack under it.
(D-Return) discards everything above it — context frames, pending `endscope`
markers and loop boundaries alike (§6.9) (helper). -/
def Kont.toCall : List Kont → Option (Frame × List Kont)
  | [] => none
  | .call φ :: K => some (φ, K)
  | _ :: K => Kont.toCall K

/-- The nearest `loopβ(e, φ)` below the top, provided no `ret(E, φ)` comes
first: "a `break` in a callee would be ill-formed" (§6.10), so a `break` with
no loop in its own frame has no rule (helper). -/
def Kont.toLoop : List Kont → Option (Frame × List Kont)
  | [] => none
  | .loop _ φ :: K => some (φ, K)
  | .call _ :: _ => none
  | _ :: K => Kont.toLoop K

/-- The root cell of a place: `ρ(root(p)) = ℓ` and `H(ℓ)` live (§6.3). An
unbound index is `unbound` and a retired cell is `useAfterDrop`, §6's stuck
states for both (helper). -/
def rootCell (H : Store) (φ : Frame) (i : Nat) : Except Violation (Nat × Contents) :=
  match φ.env[i]? with
  | none => .error .unbound
  | some ℓ =>
    match H[ℓ]? with
    | none => .error .unbound
    | some .dead => .error .useAfterDrop
    | some (.full c) => .ok (ℓ, c)

/-- `drop-retire(H, ℓ)` (§6.1) as §6 writes it: run the binding's drop and
retire the allocation, with **no** leak monitor — `dropRetire`
(`Dynamics.lean`) without its `residualLinear` test (helper). -/
def plainDropRetire (D : Decls) (H : Store) (ℓ : Nat) : Except Violation (Store × List Event) :=
  match H[ℓ]? with
  | none => .error .unbound
  | some .dead => .error .useAfterDrop
  | some (.full c) =>
      match dropCell D ℓ c with
      | .error w => .error w
      | .ok evs => .ok (H.set ℓ .dead, evs)

/-- `run-scope-drops` over a list of cells in the order given (§6.1), with no
leak monitor: `unwindLocs` (`Dynamics.lean`) over `plainDropRetire`. (D-EndScope),
(D-Return-Value), (D-Return) and (D-Break) all run it (helper). -/
def plainUnwind (D : Decls) (H : Store) : List Nat → Except Violation (Store × List Event)
  | [] => .ok (H, [])
  | ℓ :: rest =>
      match plainDropRetire D H ℓ with
      | .error w => .error w
      | .ok (H₁, evs) =>
          match plainUnwind D H₁ rest with
          | .error w => .error w
          | .ok (H₂, evs') => .ok (H₂, evs ++ evs')

/-- §6.3's `destructure(H, ℓ@π_d, π_s)` as §6.3 writes it: `split`, then
`drop*` on the residue left to right, with no residue monitor — the
(Use-Declared-Linear-Destructure) premise excluded a linear residue before
(D-Use-Declared-Linear) can fire (helper). -/
def plainDestructure (D : Decls) (c : Contents) (πs : List Nat) :
    Except Violation (Contents × List Event) :=
  match Contents.splitResidue D c πs with
  | .error w => .error w
  | .ok (leaf, rs) =>
      match dropContentsList D rs with
      | .error w => .error w
      | .ok evs => .ok (leaf, evs)

/-! ## The relation -/

/-- **§6's reduction relation** `C → C'`, over the fragment. One constructor
per §6 rule (or per rule group, where §6.4's operator tables are one function
of the operands), plus §6.2's (Search) as an *enter* and a *plug* constructor
per evaluation-context production. (Panic-Lift) is the shape of every trap
constructor. `M` fixes the float operations, as `eval`'s does; `P` supplies the
declarations and the functions. -/
inductive Step (M : FloatOps) (P : Program) : Config → Config → Prop where
  -- ### §6.3: literals and the use of a place
  /-- An integer literal is already a value (§6.3): it takes no step except to
  *be* one, `n_T` at the type elaboration resolved. -/
  | intLit {H φ K tr w s n} :
      Step M P (.run H φ K (.eval (.intLit w s n)) tr) (.run H φ K (.ret (.int w s n)) tr)
  /-- A float literal is a value (§6.3): the datum `rnd_w` of its decimal
  (`3.12:9`). -/
  | floatLit {H φ K tr w l} :
      Step M P (.run H φ K (.eval (.floatLit w l)) tr)
        (.run H φ K (.ret (.float w (M.ofLit w l.sig l.negExp l.e))) tr)
  /-- A boolean literal is a value (§6.3). -/
  | boolLit {H φ K tr b} :
      Step M P (.run H φ K (.eval (.boolLit b)) tr) (.run H φ K (.ret (.bool b)) tr)
  /-- The unit literal is a value (§6.3). -/
  | unitLit {H φ K tr} :
      Step M P (.run H φ K (.eval .unitLit) tr) (.run H φ K (.ret .unit) tr)
  /-- (D-Use-Declared-Linear) §6.3: the path has a declared-linear proper
  prefix `d` (`declaredPlan`); `destructure` splits the aggregate at `d`, drops
  the residue left to right, and `ℓ@π_d` — the consumed place — becomes `⊘`. -/
  | useDeclared {H φ K tr p ℓ c πd πs cd leaf evs v c'} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.declaredPlan P.decls p.path = some (πd, πs) →
      c.readAt πd = .ok cd →
      plainDestructure P.decls cd πs = .ok (leaf, evs) →
      leaf.toVal = some v →
      c.writeAt πd .hole = some c' →
      Step M P (.run H φ K (.eval (.use p)) tr)
        (.run (H.set ℓ (.full c')) φ K (.ret v) (tr ++ evs))
  /-- (D-Use-Copy) §6.3: an `Ordinary` use of a `Copy` place reads it and
  leaves the cell untouched. -/
  | useCopy {H φ K tr p ℓ c sub v} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.declaredPlan P.decls p.path = none →
      c.readAt p.path = .ok sub →
      sub.toVal = some v →
      v.mult P.decls = .copy →
      Step M P (.run H φ K (.eval (.use p)) tr) (.run H φ K (.ret v) tr)
  /-- (D-Use-Move) §6.3: an `Ordinary` use of an `Affine` or `Linear` place
  moves it, writing `⊘` at exactly the sub-position moved — the partial move
  of §4.2 (`3.8:22`). -/
  | useMove {H φ K tr p ℓ c sub v c'} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.declaredPlan P.decls p.path = none →
      c.readAt p.path = .ok sub →
      sub.toVal = some v →
      v.mult P.decls ≠ .copy →
      c.writeAt p.path .hole = some c' →
      Step M P (.run H φ K (.eval (.use p)) tr)
        (.run (H.set ℓ (.full c')) φ K (.ret v) tr)
  -- ### §6.4: operators and intrinsics
  /-- (Search) §6.2 into `E ⊕ e`: the left operand first. -/
  | binopEnter {H φ K tr op e₁ e₂} :
      Step M P (.run H φ K (.eval (.binop op e₁ e₂)) tr)
        (.run H φ (.binopL op e₂ :: K) (.eval e₁) tr)
  /-- (Search) §6.2 from `E ⊕ e` to `v ⊕ E`: the right operand next. -/
  | binopMid {H φ K tr op e₂ v₁} :
      Step M P (.run H φ (.binopL op e₂ :: K) (.ret v₁) tr)
        (.run H φ (.binopR op v₁ :: K) (.eval e₂) tr)
  /-- §6.4's binary rules on two values: (D-Arith), (D-Div), (D-Bit), (D-Shl),
  (D-Shr), the integer compares, (D-Float-Arith), (D-Float-Ord) and
  (D-Total-Cmp), computed by `evalBinOp` (`Dynamics.lean`). -/
  | binop {H φ K tr op v₁ v₂ v} :
      evalBinOp M op v₁ v₂ = .val v →
      Step M P (.run H φ (.binopR op v₁ :: K) (.ret v₂) tr) (.run H φ K (.ret v) tr)
  /-- §6.4's binary traps, (D-Arith-Trap), (D-Div-Zero) and (D-Div-Overflow)
  with `%`'s `rem-zero`, lifted past every context by (Panic-Lift) §6.2. -/
  | binopTrap {H φ K tr op v₁ v₂ κ} :
      evalBinOp M op v₁ v₂ = .trap κ →
      Step M P (.run H φ (.binopR op v₁ :: K) (.ret v₂) tr) (.panic κ tr)
  /-- (Search) §6.2 into `⊖ E`. -/
  | unopEnter {H φ K tr op e} :
      Step M P (.run H φ K (.eval (.unop op e)) tr) (.run H φ (.unop op :: K) (.eval e) tr)
  /-- §6.4's unary rules: (D-Arith)'s unary `neg`, (D-Float-Neg), `not`, and
  (D-Bit)'s complement (`evalUnOp`). -/
  | unop {H φ K tr op v v'} :
      evalUnOp op v = .val v' →
      Step M P (.run H φ (.unop op :: K) (.ret v) tr) (.run H φ K (.ret v') tr)
  /-- (D-Arith-Trap) §6.4 at `neg (min_T)`, lifted by (Panic-Lift) §6.2. -/
  | unopTrap {H φ K tr op v κ} :
      evalUnOp op v = .trap κ →
      Step M P (.run H φ (.unop op :: K) (.ret v) tr) (.panic κ tr)
  /-- (Search) §6.2 into `@intCast( E )`. -/
  | intCastEnter {H φ K tr w s e} :
      Step M P (.run H φ K (.eval (.intCast w s e)) tr)
        (.run H φ (.intCast w s :: K) (.eval e) tr)
  /-- (D-Int-Cast) §6.4. -/
  | intCast {H φ K tr w s v v'} :
      evalIntCast w s v = .val v' →
      Step M P (.run H φ (.intCast w s :: K) (.ret v) tr) (.run H φ K (.ret v') tr)
  /-- (D-Int-Cast-Trap) §6.4, lifted by (Panic-Lift) §6.2. -/
  | intCastTrap {H φ K tr w s v κ} :
      evalIntCast w s v = .trap κ →
      Step M P (.run H φ (.intCast w s :: K) (.ret v) tr) (.panic κ tr)
  /-- (Search) §6.2 into a float intrinsic's operand, `@f( E )`. -/
  | fintrinEnter {H φ K tr k e} :
      Step M P (.run H φ K (.eval (.fintrin k e)) tr)
        (.run H φ (.fintrin k :: K) (.eval e) tr)
  /-- §6.4's float intrinsics: (D-Int-To-Float), (D-Float-To-Int),
  (D-Float-Cast) and (D-Float-Round) (`evalFintrin`). -/
  | fintrin {H φ K tr k v v'} :
      evalFintrin M k v = .val v' →
      Step M P (.run H φ (.fintrin k :: K) (.ret v) tr) (.run H φ K (.ret v') tr)
  /-- (D-Float-To-Int-Trap) §6.4, lifted by (Panic-Lift) §6.2. -/
  | fintrinTrap {H φ K tr k v κ} :
      evalFintrin M k v = .trap κ →
      Step M P (.run H φ (.fintrin k :: K) (.ret v) tr) (.panic κ tr)
  -- ### §6.12: `@panic` and `@dbg`
  /-- (D-Panic) §6.12: `@panic` abandons the configuration to `↯user`, which
  (Panic-Lift) §6.2 carries past every context. The fragment's message is a
  literal field, so there is no operand to reduce first. -/
  | panic {H φ K tr msg} :
      Step M P (.run H φ K (.eval (.panic msg)) tr) (.panic .user tr)
  /-- (Search) §6.2 into `@dbg( E )`. -/
  | dbgEnter {H φ K tr e} :
      Step M P (.run H φ K (.eval (.dbg e)) tr) (.run H φ (.dbg :: K) (.eval e) tr)
  /-- `@dbg`'s defining equation (§6.9's intrinsic note, §6.12): append the
  value's rendering to the observable output and yield `⟨⟩`. -/
  | dbg {H φ K tr v} :
      Step M P (.run H φ (.dbg :: K) (.ret v) tr) (.run H φ K (.ret .unit) (tr ++ [.dbg v]))
  -- ### §6.2's list contexts
  /-- (Search) §6.2 into a struct literal's initializers, `S{ E, ē }`. -/
  | structEnter {H φ K tr s args} :
      Step M P (.run H φ K (.eval (.mkStruct s args)) tr) (.run H φ K (.args (.struct s) [] args) tr)
  /-- (Search) §6.2 into an enum literal's payload, `Kj( E, ē )`. -/
  | enumEnter {H φ K tr e k args} :
      Step M P (.run H φ K (.eval (.mkEnum e k args)) tr)
        (.run H φ K (.args (.enum e k) [] args) tr)
  /-- (Search) §6.2 into an array literal's elements, `[ E, ē ]`. -/
  | arrayEnter {H φ K tr T args} :
      Step M P (.run H φ K (.eval (.mkArray T args)) tr) (.run H φ K (.args (.array T) [] args) tr)
  /-- (Search) §6.2 into a call's by-value arguments, `g( E, ē )`. -/
  | callEnter {H φ K tr f args} :
      Step M P (.run H φ K (.eval (.call f args)) tr) (.run H φ K (.args (.call f) [] args) tr)
  /-- (Search) §6.2 into a dynamic place's indices, `p[ E, ē ]`: "a place …
  is a redex once its index subexpressions are values". -/
  | indexReadEnter {H φ K tr p idx πs} :
      Step M P (.run H φ K (.eval (.indexRead p idx πs)) tr)
        (.run H φ K (.args (.indexRead p πs) [] idx) tr)
  /-- (Search) §6.2 into the indices of `@drop` at a dynamic place. -/
  | indexDropEnter {H φ K tr p idx πs} :
      Step M P (.run H φ K (.eval (.indexDrop p idx πs)) tr)
        (.run H φ K (.args (.indexDrop p πs) [] idx) tr)
  /-- (Search) §6.2 into an assignment below a dynamic index: `assign p = E`,
  the right-hand side **first** (`5.2:14`). -/
  | indexWriteEnter {H φ K tr p idx πs e} :
      Step M P (.run H φ K (.eval (.indexWrite p idx πs e)) tr)
        (.run H φ (.indexWriteRhs p idx πs :: K) (.eval e) tr)
  /-- (Search) §6.2 from `assign p = E` to `assign p[ E, ē ] = v`: the
  indices next, left to right (`5.2:14`). -/
  | indexWriteRhs {H φ K tr p idx πs v} :
      Step M P (.run H φ (.indexWriteRhs p idx πs :: K) (.ret v) tr)
        (.run H φ K (.args (.indexWrite p πs v) [] idx) tr)
  /-- (Search) §6.2 into the next hole of a list context, `…( v̄, E, ē )`. -/
  | argsPush {H φ K tr t vs e es} :
      Step M P (.run H φ K (.args t vs (e :: es)) tr) (.run H φ (.args t vs es :: K) (.eval e) tr)
  /-- (Search) §6.2: a list context's hole became a value; move past it. -/
  | argsPlug {H φ K tr t vs es v} :
      Step M P (.run H φ (.args t vs es :: K) (.ret v) tr) (.run H φ K (.args t (vs ++ [v]) es) tr)
  -- ### §6.5, §6.6: aggregates, enums, `match`, `if`
  /-- (D-Struct) §6.5: every initializer is a value. -/
  | mkStruct {H φ K tr s vs sd} :
      P.decls.structs[s]? = some sd →
      sd.fields.length = vs.length →
      Step M P (.run H φ K (.args (.struct s) vs []) tr) (.run H φ K (.ret (.struct s vs)) tr)
  /-- (D-Enum-Intro) §6.6: every payload component is a value. -/
  | mkEnum {H φ K tr e k vs ed Ts} :
      P.decls.enums[e]? = some ed →
      ed.variants[k]? = some Ts →
      Ts.length = vs.length →
      Step M P (.run H φ K (.args (.enum e k) vs []) tr) (.run H φ K (.ret (.enum e k vs)) tr)
  /-- (D-Array) §6.5: every element is a value. -/
  | mkArray {H φ K tr T vs} :
      Step M P (.run H φ K (.args (.array T) vs []) tr) (.run H φ K (.ret (.array T vs)) tr)
  /-- (Search) §6.2 into the repeat form's operand (`7.1:39`). -/
  | repeatEnter {H φ K tr T e n} :
      Step M P (.run H φ K (.eval (.repeatArray T e n)) tr)
        (.run H φ (.repeatArray T n :: K) (.eval e) tr)
  /-- The repeat form's elaboration (`7.1:39`): the operand, evaluated once,
  copied into each of the `n` slots. -/
  | repeatArray {H φ K tr T n v} :
      Step M P (.run H φ (.repeatArray T n :: K) (.ret v) tr)
        (.run H φ K (.ret (.array T (List.replicate n v))) tr)
  /-- (D-Index) §6.5 at a dynamic place, every index in range, and
  (D-Use-Untrackable-Dynamic-Copy) §6.3 reads the `Copy` leaf, leaving the
  storage live. -/
  | indexRead {H φ K tr p πs vs ℓ c sub ρ leaf v} :
      dynPlace H φ p vs πs = .at ℓ c sub ρ →
      sub.readAt ρ = .ok leaf →
      leaf.toVal = some v →
      Step M P (.run H φ K (.args (.indexRead p πs) vs []) tr) (.run H φ K (.ret v) tr)
  /-- (D-Index-Trap) §6.5: an index out of range traps `↯bounds`, lifted by
  (Panic-Lift) §6.2. -/
  | indexReadTrap {H φ K tr p πs vs} :
      dynPlace H φ p vs πs = .bounds →
      Step M P (.run H φ K (.args (.indexRead p πs) vs []) tr) (.panic .bounds tr)
  /-- §6.11's `@drop` at a `Copy` place below a dynamic index: (D-Index) §6.5
  navigates it, nothing is dropped, and the result is `⟨⟩`. -/
  | indexDrop {H φ K tr p πs vs ℓ c sub ρ leaf v} :
      dynPlace H φ p vs πs = .at ℓ c sub ρ →
      sub.readAt ρ = .ok leaf →
      leaf.toVal = some v →
      Step M P (.run H φ K (.args (.indexDrop p πs) vs []) tr) (.run H φ K (.ret .unit) tr)
  /-- (D-Index-Trap) §6.5 at `@drop`'s dynamic place. -/
  | indexDropTrap {H φ K tr p πs vs} :
      dynPlace H φ p vs πs = .bounds →
      Step M P (.run H φ K (.args (.indexDrop p πs) vs []) tr) (.panic .bounds tr)
  /-- (D-Assign) §6.8 below a dynamic index, every index in range (D-Index)
  §6.5: overwrite-drop what the position holds, then store. -/
  | indexWrite {H φ K tr p πs v vs ℓ c sub ρ old evs sub' c'} :
      dynPlace H φ p vs πs = .at ℓ c sub ρ →
      sub.readAt ρ = .ok old →
      dropCell P.decls ℓ old = .ok evs →
      sub.writeAt ρ (Contents.ofVal v) = some sub' →
      c.writeAt p.path sub' = some c' →
      Step M P (.run H φ K (.args (.indexWrite p πs v) vs []) tr)
        (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))
  /-- (D-Index-Trap) §6.5 at an assignment's dynamic place: the evaluated
  right-hand side is abandoned undropped (§6.12: a panic runs no drops). -/
  | indexWriteTrap {H φ K tr p πs v vs} :
      dynPlace H φ p vs πs = .bounds →
      Step M P (.run H φ K (.args (.indexWrite p πs v) vs []) tr) (.panic .bounds tr)
  /-- (Search) §6.2 into `match E { … }`. -/
  | matchEnter {H φ K tr scrut arms} :
      Step M P (.run H φ K (.eval (.«match» scrut arms)) tr)
        (.run H φ (.«match» arms :: K) (.eval scrut) tr)
  /-- (D-Match) §6.6: the tag selects the arm; the payload is bound to fresh
  cells, appended to the scope record *and* owed to the arm's `endscope`. -/
  | «match» {H φ K tr arms e k vs body H' ls} :
      arms[k]? = some body →
      mintParams H vs = (H', ls) →
      Step M P (.run H φ (.«match» arms :: K) (.ret (.enum e k vs)) tr)
        (.run H' { env := ls.reverse ++ φ.env, scope := φ.scope ++ ls }
          (.endscope ls φ :: K) (.eval body) tr)
  /-- (Search) §6.2 into `if E { e1 } else { e2 }`. -/
  | iteEnter {H φ K tr c e₁ e₂} :
      Step M P (.run H φ K (.eval (.ite c e₁ e₂)) tr) (.run H φ (.ite e₁ e₂ :: K) (.eval c) tr)
  /-- (D-If-T) §6.6. -/
  | iteTrue {H φ K tr e₁ e₂} :
      Step M P (.run H φ (.ite e₁ e₂ :: K) (.ret (.bool true)) tr) (.run H φ K (.eval e₁) tr)
  /-- (D-If-F) §6.6. -/
  | iteFalse {H φ K tr e₁ e₂} :
      Step M P (.run H φ (.ite e₁ e₂ :: K) (.ret (.bool false)) tr) (.run H φ K (.eval e₂) tr)
  -- ### §6.7, §6.8: `let`, `endscope`, sequencing, assignment, `@drop`
  /-- (Search) §6.2 into `let x = E ; e2`. -/
  | letEnter {H φ K tr m e₁ e₂} :
      Step M P (.run H φ K (.eval (.letIn m e₁ e₂)) tr) (.run H φ (.letIn e₂ :: K) (.eval e₁) tr)
  /-- (D-Let) §6.7: a fresh cell, bound, appended to the innermost scope
  record, and owed to the body's `endscope` (RUE-1277). -/
  | letBind {H φ K tr e₂ v} :
      Step M P (.run H φ (.letIn e₂ :: K) (.ret v) tr)
        (.run (H ++ [.full (Contents.ofVal v)])
          { env := H.length :: φ.env, scope := φ.scope ++ [H.length] }
          (.endscope [H.length] φ :: K) (.eval e₂) tr)
  /-- (D-EndScope) §6.7: the body is a value; drop-retire the marker's cells
  newest-first and resume the enclosing frame. -/
  | endScope {H φ K tr ℓs φs v H' evs} :
      plainUnwind P.decls H ℓs.reverse = .ok (H', evs) →
      Step M P (.run H φ (.endscope ℓs φs :: K) (.ret v) tr) (.run H' φs K (.ret v) (tr ++ evs))
  /-- (Search) §6.2 into `E ; e2`. -/
  | seqEnter {H φ K tr e₁ e₂} :
      Step M P (.run H φ K (.eval (.seq e₁ e₂)) tr) (.run H φ (.seq e₂ :: K) (.eval e₁) tr)
  /-- (D-Seq) §6.7 at a `Copy` temporary: `drop(H, v)` is `H`. -/
  | seqCopy {H φ K tr e₂ v} :
      v.mult P.decls = .copy →
      Step M P (.run H φ (.seq e₂ :: K) (.ret v) tr) (.run H φ K (.eval e₂) tr)
  /-- (D-Seq) §6.7 at a droppable temporary: drop it, then continue. §5.3
  guarantees it carries no linear value; the rule does not check. -/
  | seqDrop {H φ K tr e₂ v evs} :
      v.mult P.decls ≠ .copy →
      dropContents P.decls (Contents.ofVal v) = .ok evs →
      Step M P (.run H φ (.seq e₂ :: K) (.ret v) tr)
        (.run H φ K (.eval e₂) (tr ++ (.dropTemp v :: evs)))
  /-- (Search) §6.2 into `assign p = E`. -/
  | assignEnter {H φ K tr p e} :
      Step M P (.run H φ K (.eval (.assign p e)) tr) (.run H φ (.assign p :: K) (.eval e) tr)
  /-- (D-Assign) §6.8: overwrite-drop what the position holds (nothing for a
  `⊘`, which is reinitialisation), then store. No linearity premise: §5.2's
  (Assign) excluded a live linear position statically (`3.8:77`). -/
  | assign {H φ K tr p v ℓ c old evs c'} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.readAt p.path = .ok old →
      dropCell P.decls ℓ old = .ok evs →
      c.writeAt p.path (Contents.ofVal v) = some c' →
      Step M P (.run H φ (.assign p :: K) (.ret v) tr)
        (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))
  /-- §6.11's `@drop` at a declared-linear plan: §6.3's destructure, then the
  selected leaf's own drop, and `⊘` at the consumed place. -/
  | dropDeclared {H φ K tr p ℓ c πd πs cd leaf evs levs c'} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.declaredPlan P.decls p.path = some (πd, πs) →
      c.readAt πd = .ok cd →
      plainDestructure P.decls cd πs = .ok (leaf, evs) →
      leaf.isHole = false →
      dropCell P.decls ℓ leaf = .ok levs →
      c.writeAt πd .hole = some c' →
      Step M P (.run H φ K (.eval (.drop p)) tr)
        (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ (evs ++ levs)))
  /-- §6.11's `@drop` of a `Copy` place: `⟨⟩`, the store unchanged. -/
  | dropCopy {H φ K tr p ℓ c sub} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.declaredPlan P.decls p.path = none →
      c.readAt p.path = .ok sub →
      sub.isHole = false →
      sub.mult P.decls = .copy →
      Step M P (.run H φ K (.eval (.drop p)) tr) (.run H φ K (.ret .unit) tr)
  /-- §6.11's `@drop` of a non-`Copy` place: run `drop` on what it holds (the
  walk skips every `⊘` inside), then write `⊘` back. -/
  | dropMove {H φ K tr p ℓ c sub evs c'} :
      rootCell H φ p.root = .ok (ℓ, c) →
      c.declaredPlan P.decls p.path = none →
      c.readAt p.path = .ok sub →
      sub.isHole = false →
      sub.mult P.decls ≠ .copy →
      dropCell P.decls ℓ sub = .ok evs →
      c.writeAt p.path .hole = some c' →
      Step M P (.run H φ K (.eval (.drop p)) tr)
        (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))
  -- ### §6.9: calls and `return`
  /-- (D-Call) §6.9: every argument is a value; mint one cell per by-value
  argument, suspend the caller as `ret(E, φ)`, and enter the body in the
  callee's frame, whose entry scope owes exactly those cells. -/
  | call {H φ K tr f vs fd H' ls} :
      P.fns[f]? = some fd →
      fd.params.length = vs.length →
      mintParams H vs = (H', ls) →
      Step M P (.run H φ K (.args (.call f) vs []) tr)
        (.run H' { env := ls.reverse, scope := ls } (.call φ :: K) (.eval fd.body) tr)
  /-- (D-Return-Value) §6.9 — and (D-Return-Main) at the bottom of the stack:
  the body is a value; run the frame's scope drops and resume the caller. -/
  | callReturn {H φ K tr φs v H' evs} :
      plainUnwind P.decls H φ.scope.reverse = .ok (H', evs) →
      Step M P (.run H φ (.call φs :: K) (.ret v) tr) (.run H' φs K (.ret v) (tr ++ evs))
  /-- (Search) §6.2 into `return E`. -/
  | retEnter {H φ K tr e} :
      Step M P (.run H φ K (.eval (.ret e)) tr) (.run H φ (.ret :: K) (.eval e) tr)
  /-- (D-Return) §6.9: discard every frame up to the nearest `ret(E, φ)` —
  pending `endscope` markers and loop boundaries included — run the frame's
  scope drops from its record, and hand `v` to the caller. -/
  | ret {H φ K tr v φs K' H' evs} :
      Kont.toCall K = some (φs, K') →
      plainUnwind P.decls H φ.scope.reverse = .ok (H', evs) →
      Step M P (.run H φ (.ret :: K) (.ret v) tr) (.run H' φs K' (.ret v) (tr ++ evs))
  -- ### §6.10: `loop` and `break`
  /-- (D-Loop-Enter) §6.10: push the loop boundary and enter the body. -/
  | loopEnter {H φ K tr e} :
      Step M P (.run H φ K (.eval (.loop e)) tr) (.run H φ (.loop e φ :: K) (.eval e) tr)
  /-- (D-Loop-Iter) §6.10: the body became a value; re-enter it in the loop's
  frame. -/
  | loopIter {H φ K tr e φs v} :
      Step M P (.run H φ (.loop e φs :: K) (.ret v) tr)
        (.run H φs (.loop e φs :: K) (.eval e) tr)
  /-- (D-Break) §6.10: discard every frame up to the nearest loop boundary,
  drop-retire the cells the body still owed newest-first
  (`unwind-drops(H, φ', φ)`), and yield `⟨⟩` to the loop's context. -/
  | brk {H φ K tr φs K' H' evs} :
      Kont.toLoop K = some (φs, K') →
      plainUnwind P.decls H (φ.scope.drop φs.scope.length).reverse = .ok (H', evs) →
      Step M P (.run H φ K (.eval .brk) tr) (.run H' φs K' (.ret .unit) (tr ++ evs))

/-- `→*` (§6.12), the reflexive-transitive closure of `Step`. -/
inductive Steps (M : FloatOps) (P : Program) : Config → Config → Prop where
  | refl (C : Config) : Steps M P C C
  | step {C₁ C₂ C₃ : Config} : Step M P C₁ C₂ → Steps M P C₂ C₃ → Steps M P C₁ C₃

/-! ## The relation as a function

`step` computes the one configuration `Step` allows, or says why there is
none. Its purpose is the enumeration the issue asks for: every configuration is
terminal, takes a step, or is stuck on one of §6's own violations, and
`step_iff` is the proof that the function and the relation agree. -/

/-- What `step` finds at a configuration: the next one, a terminal one
(`Config.Terminal`), or a stuck one, named by the `Violation` §6 leaves it
at (helper). -/
inductive StepOut where
  | next (C : Config)
  | halted
  | stuck (w : Violation)
deriving Repr

/-- `step` at an expression in focus: the literal rules of §6.3, the place
rules of §6.3 and §6.11, (D-Panic) §6.12, (D-Loop-Enter) and (D-Break)
§6.10, and every (Search) enter rule of §6.2 (helper). -/
def stepEval (M : FloatOps) (P : Program) (H : Store) (φ : Frame) (K : List Kont)
    (tr : List Event) : Expr → StepOut
  | .intLit w s n => .next (.run H φ K (.ret (.int w s n)) tr)
  | .floatLit w l => .next (.run H φ K (.ret (.float w (M.ofLit w l.sig l.negExp l.e))) tr)
  | .boolLit b => .next (.run H φ K (.ret (.bool b)) tr)
  | .unitLit => .next (.run H φ K (.ret .unit) tr)
  | .use p =>
    match rootCell H φ p.root with
    | .error w => .stuck w
    | .ok (ℓ, c) =>
      match c.declaredPlan P.decls p.path with
      | some (πd, πs) =>
        match c.readAt πd with
        | .error w => .stuck w
        | .ok cd =>
          match plainDestructure P.decls cd πs with
          | .error w => .stuck w
          | .ok (leaf, evs) =>
            match leaf.toVal with
            | none => .stuck .useAfterMove
            | some v =>
              match c.writeAt πd .hole with
              | none => .stuck .typeConfusion
              | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret v) (tr ++ evs))
      | none =>
        match c.readAt p.path with
        | .error w => .stuck w
        | .ok sub =>
          match sub.toVal with
          | none => .stuck .useAfterMove
          | some v =>
            if v.mult P.decls = .copy then .next (.run H φ K (.ret v) tr)
            else
              match c.writeAt p.path .hole with
              | none => .stuck .typeConfusion
              | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret v) tr)
  | .binop op e₁ e₂ => .next (.run H φ (.binopL op e₂ :: K) (.eval e₁) tr)
  | .unop op e => .next (.run H φ (.unop op :: K) (.eval e) tr)
  | .intCast w s e => .next (.run H φ (.intCast w s :: K) (.eval e) tr)
  | .fintrin k e => .next (.run H φ (.fintrin k :: K) (.eval e) tr)
  | .panic _ => .next (.panic .user tr)
  | .dbg e => .next (.run H φ (.dbg :: K) (.eval e) tr)
  | .mkStruct s args => .next (.run H φ K (.args (.struct s) [] args) tr)
  | .mkEnum e k args => .next (.run H φ K (.args (.enum e k) [] args) tr)
  | .«match» scrut arms => .next (.run H φ (.«match» arms :: K) (.eval scrut) tr)
  | .mkArray T args => .next (.run H φ K (.args (.array T) [] args) tr)
  | .repeatArray T e n => .next (.run H φ (.repeatArray T n :: K) (.eval e) tr)
  | .indexRead p idx πs => .next (.run H φ K (.args (.indexRead p πs) [] idx) tr)
  | .indexWrite p idx πs e => .next (.run H φ (.indexWriteRhs p idx πs :: K) (.eval e) tr)
  | .indexDrop p idx πs => .next (.run H φ K (.args (.indexDrop p πs) [] idx) tr)
  | .drop p =>
    match rootCell H φ p.root with
    | .error w => .stuck w
    | .ok (ℓ, c) =>
      match c.declaredPlan P.decls p.path with
      | some (πd, πs) =>
        match c.readAt πd with
        | .error w => .stuck w
        | .ok cd =>
          match plainDestructure P.decls cd πs with
          | .error w => .stuck w
          | .ok (leaf, evs) =>
            if leaf.isHole then .stuck .useAfterMove
            else
              match dropCell P.decls ℓ leaf with
              | .error w => .stuck w
              | .ok levs =>
                match c.writeAt πd .hole with
                | none => .stuck .typeConfusion
                | some c' =>
                  .next (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ (evs ++ levs)))
      | none =>
        match c.readAt p.path with
        | .error w => .stuck w
        | .ok sub =>
          if sub.isHole then .stuck .useAfterMove
          else if sub.mult P.decls = .copy then .next (.run H φ K (.ret .unit) tr)
          else
            match dropCell P.decls ℓ sub with
            | .error w => .stuck w
            | .ok evs =>
              match c.writeAt p.path .hole with
              | none => .stuck .typeConfusion
              | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))
  | .letIn _ e₁ e₂ => .next (.run H φ (.letIn e₂ :: K) (.eval e₁) tr)
  | .assign p e => .next (.run H φ (.assign p :: K) (.eval e) tr)
  | .seq e₁ e₂ => .next (.run H φ (.seq e₂ :: K) (.eval e₁) tr)
  | .ite c e₁ e₂ => .next (.run H φ (.ite e₁ e₂ :: K) (.eval c) tr)
  | .call f args => .next (.run H φ K (.args (.call f) [] args) tr)
  | .ret e => .next (.run H φ (.ret :: K) (.eval e) tr)
  | .loop e => .next (.run H φ (.loop e φ :: K) (.eval e) tr)
  | .brk =>
    match Kont.toLoop K with
    | none => .stuck .typeConfusion
    | some (φs, K') =>
      match plainUnwind P.decls H (φ.scope.drop φs.scope.length).reverse with
      | .error w => .stuck w
      | .ok (H', evs) => .next (.run H' φs K' (.ret .unit) (tr ++ evs))

/-- `step` at a completed argument list: (D-Struct), (D-Enum-Intro),
(D-Array), (D-Call), and (D-Index)/(D-Index-Trap) with (D-Assign) at a
dynamic place (helper). -/
def stepArgs (P : Program) (H : Store) (φ : Frame) (K : List Kont) (tr : List Event)
    (vs : List Val) : ArgsTag → StepOut
  | .struct s =>
    match P.decls.structs[s]? with
    | none => .stuck .unbound
    | some sd =>
      if sd.fields.length = vs.length then .next (.run H φ K (.ret (.struct s vs)) tr)
      else .stuck .typeConfusion
  | .enum e k =>
    match P.decls.enums[e]? with
    | none => .stuck .unbound
    | some ed =>
      match ed.variants[k]? with
      | none => .stuck .typeConfusion
      | some Ts =>
        if Ts.length = vs.length then .next (.run H φ K (.ret (.enum e k vs)) tr)
        else .stuck .typeConfusion
  | .array T => .next (.run H φ K (.ret (.array T vs)) tr)
  | .call f =>
    match P.fns[f]? with
    | none => .stuck .unbound
    | some fd =>
      if fd.params.length = vs.length then
        match mintParams H vs with
        | (H', ls) =>
          .next (.run H' { env := ls.reverse, scope := ls } (.call φ :: K) (.eval fd.body) tr)
      else .stuck .typeConfusion
  | .indexRead p πs =>
    match dynPlace H φ p vs πs with
    | .stuck w => .stuck w
    | .bounds => .next (.panic .bounds tr)
    | .at _ _ sub ρ =>
      match sub.readAt ρ with
      | .error w => .stuck w
      | .ok leaf =>
        match leaf.toVal with
        | none => .stuck .useAfterMove
        | some v => .next (.run H φ K (.ret v) tr)
  | .indexDrop p πs =>
    match dynPlace H φ p vs πs with
    | .stuck w => .stuck w
    | .bounds => .next (.panic .bounds tr)
    | .at _ _ sub ρ =>
      match sub.readAt ρ with
      | .error w => .stuck w
      | .ok leaf =>
        match leaf.toVal with
        | none => .stuck .useAfterMove
        | some _ => .next (.run H φ K (.ret .unit) tr)
  | .indexWrite p πs v =>
    match dynPlace H φ p vs πs with
    | .stuck w => .stuck w
    | .bounds => .next (.panic .bounds tr)
    | .at ℓ c sub ρ =>
      match sub.readAt ρ with
      | .error w => .stuck w
      | .ok old =>
        match dropCell P.decls ℓ old with
        | .error w => .stuck w
        | .ok evs =>
          match sub.writeAt ρ (Contents.ofVal v) with
          | none => .stuck .typeConfusion
          | some sub' =>
            match c.writeAt p.path sub' with
            | none => .stuck .typeConfusion
            | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))

/-- An operator's outcome as a step: a value plugs the hole, a trap is
(Panic-Lift) §6.2, and a wrong-shaped operand is stuck (helper). -/
def OpRes.toStep (H : Store) (φ : Frame) (K : List Kont) (tr : List Event) : OpRes → StepOut
  | .val v => .next (.run H φ K (.ret v) tr)
  | .trap κ => .next (.panic κ tr)
  | .confused => .stuck .typeConfusion

/-- `step` at a value returning into the top frame: every (Search) plug rule
of §6.2 and the redexes that fire there — §6.4's operators, (D-Match),
(D-If-T)/(D-If-F), (D-Let), (D-EndScope), (D-Seq), (D-Assign),
(D-Return-Value), (D-Return) and (D-Loop-Iter) (helper). -/
def stepRet (M : FloatOps) (P : Program) (H : Store) (φ : Frame) (K : List Kont)
    (tr : List Event) (v : Val) : Kont → StepOut
  | .binopL op e₂ => .next (.run H φ (.binopR op v :: K) (.eval e₂) tr)
  | .binopR op v₁ => (evalBinOp M op v₁ v).toStep H φ K tr
  | .unop op => (evalUnOp op v).toStep H φ K tr
  | .intCast w s => (evalIntCast w s v).toStep H φ K tr
  | .fintrin k => (evalFintrin M k v).toStep H φ K tr
  | .dbg => .next (.run H φ K (.ret .unit) (tr ++ [.dbg v]))
  | .args t vs es => .next (.run H φ K (.args t (vs ++ [v]) es) tr)
  | .repeatArray T n => .next (.run H φ K (.ret (.array T (List.replicate n v))) tr)
  | .indexWriteRhs p idx πs => .next (.run H φ K (.args (.indexWrite p πs v) [] idx) tr)
  | .«match» arms =>
    match v with
    | .enum _ k vs =>
      match arms[k]? with
      | none => .stuck .typeConfusion
      | some body =>
        match mintParams H vs with
        | (H', ls) =>
          .next (.run H' { env := ls.reverse ++ φ.env, scope := φ.scope ++ ls }
            (.endscope ls φ :: K) (.eval body) tr)
    | _ => .stuck .typeConfusion
  | .letIn e₂ =>
    .next (.run (H ++ [.full (Contents.ofVal v)])
      { env := H.length :: φ.env, scope := φ.scope ++ [H.length] }
      (.endscope [H.length] φ :: K) (.eval e₂) tr)
  | .seq e₂ =>
    if v.mult P.decls = .copy then .next (.run H φ K (.eval e₂) tr)
    else
      match dropContents P.decls (Contents.ofVal v) with
      | .error w => .stuck w
      | .ok evs => .next (.run H φ K (.eval e₂) (tr ++ (.dropTemp v :: evs)))
  | .ite e₁ e₂ =>
    match v with
    | .bool true => .next (.run H φ K (.eval e₁) tr)
    | .bool false => .next (.run H φ K (.eval e₂) tr)
    | _ => .stuck .typeConfusion
  | .assign p =>
    match rootCell H φ p.root with
    | .error w => .stuck w
    | .ok (ℓ, c) =>
      match c.readAt p.path with
      | .error w => .stuck w
      | .ok old =>
        match dropCell P.decls ℓ old with
        | .error w => .stuck w
        | .ok evs =>
          match c.writeAt p.path (Contents.ofVal v) with
          | none => .stuck .typeConfusion
          | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))
  | .ret =>
    match Kont.toCall K with
    | none => .stuck .typeConfusion
    | some (φs, K') =>
      match plainUnwind P.decls H φ.scope.reverse with
      | .error w => .stuck w
      | .ok (H', evs) => .next (.run H' φs K' (.ret v) (tr ++ evs))
  | .endscope ℓs φs =>
    match plainUnwind P.decls H ℓs.reverse with
    | .error w => .stuck w
    | .ok (H', evs) => .next (.run H' φs K (.ret v) (tr ++ evs))
  | .loop e φs => .next (.run H φs (.loop e φs :: K) (.eval e) tr)
  | .call φs =>
    match plainUnwind P.decls H φ.scope.reverse with
    | .error w => .stuck w
    | .ok (H', evs) => .next (.run H' φs K (.ret v) (tr ++ evs))

/-- **`Step` as a function**: the one step a configuration takes, or why it
takes none — `halted` at a terminal configuration (§6.12's (Result-Ok) and
(Result-Panic)), `stuck w` where §6 has no rule. `step_iff` is the proof that
it is `Step`. -/
def step (M : FloatOps) (P : Program) : Config → StepOut
  | .panic _ _ => .halted
  | .run H φ K (.eval e) tr => stepEval M P H φ K tr e
  | .run H φ K (.args t vs (e :: es)) tr => .next (.run H φ (.args t vs es :: K) (.eval e) tr)
  | .run H φ K (.args t vs []) tr => stepArgs P H φ K tr vs t
  | .run _ _ [] (.ret _) _ => .halted
  | .run H φ (k :: K) (.ret v) tr => stepRet M P H φ K tr v k

/-! ## The sanity theorems -/

/-- Every `Step` is the one `step` computes (§6). -/
theorem Step.step_eq {M : FloatOps} {P : Program} {C C' : Config} (h : Step M P C C') :
    step M P C = .next C' := by
  cases h <;> simp_all [step, stepEval, stepArgs, stepRet, OpRes.toStep]

/-- **Determinism** of §6's reduction on the fragment: a configuration takes
at most one step. The rules' left-hand sides fix the focus and the top frame,
and the pairs that share one — (D-Use-Copy)/(D-Use-Move)/(D-Use-Declared-Linear),
(D-Seq)'s two cases, (D-If-T)/(D-If-F), an operator's value and trap rules,
(D-Index)/(D-Index-Trap) — are split by premises that are functions of the
configuration. -/
theorem Step.det {M : FloatOps} {P : Program} {C C₁ C₂ : Config}
    (h₁ : Step M P C C₁) (h₂ : Step M P C C₂) : C₁ = C₂ := by
  have e₁ := h₁.step_eq
  rw [h₂.step_eq] at e₁
  exact (StepOut.next.inj e₁).symm

/-- A terminal configuration takes no step: `✓n` and `↯κ` are final (§6.12). -/
theorem Step.terminal {M : FloatOps} {P : Program} {C C' : Config}
    (hC : C.Terminal) : ¬ Step M P C C' := by
  intro h
  cases h <;> simp [Config.Terminal] at hC

/-! ## `step` is `Step`, and the enumeration of what a configuration can be -/

/-- `stepEval`'s `next` is a `Step` (helper). -/
theorem stepEval_complete {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {e : Expr} {C' : Config}
    (h : stepEval M P H φ K tr e = .next C') : Step M P (.run H φ K (.eval e) tr) C' := by
  cases e <;> simp only [stepEval] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (try simp only [Bool.not_eq_true] at *)
  all_goals (constructor <;> first | assumption | rfl)

/-- `stepArgs`'s `next` is a `Step` (helper). -/
theorem stepArgs_complete {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {vs : List Val} {t : ArgsTag} {C' : Config}
    (h : stepArgs P H φ K tr vs t = .next C') : Step M P (.run H φ K (.args t vs []) tr) C' := by
  cases t <;> simp only [stepArgs] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (constructor <;> first | assumption | rfl)

/-- `stepRet`'s `next` is a `Step` (helper). -/
theorem stepRet_complete {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {v : Val} {k : Kont} {C' : Config}
    (h : stepRet M P H φ K tr v k = .next C') : Step M P (.run H φ (k :: K) (.ret v) tr) C' := by
  cases k <;> simp only [stepRet, OpRes.toStep] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (constructor <;> first | assumption | rfl)

/-- **`step` is `Step`** (§6): the function computes exactly the relation's
one step. With `Step.step_eq` this is what makes `step`'s other two answers
an enumeration of the configurations that take no step. -/
theorem step_iff {M : FloatOps} {P : Program} {C C' : Config} :
    Step M P C C' ↔ step M P C = .next C' := by
  refine ⟨Step.step_eq, fun h => ?_⟩
  match C, h with
  | .panic _ _, h => simp [step] at h
  | .run H φ K (.eval e) tr, h => exact stepEval_complete h
  | .run H φ K (.args t vs (e :: es)) tr, h =>
      simp only [step, StepOut.next.injEq] at h; subst h; exact .argsPush
  | .run H φ K (.args t vs []) tr, h => exact stepArgs_complete h
  | .run _ _ [] (.ret _) _, h => simp [step] at h
  | .run H φ (k :: K) (.ret v) tr, h => exact stepRet_complete h

/-- `stepEval` never answers `halted` (helper). -/
theorem stepEval_ne_halted {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {e : Expr} : stepEval M P H φ K tr e ≠ .halted := by
  intro h
  cases e <;> simp only [stepEval] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `stepArgs` never answers `halted` (helper). -/
theorem stepArgs_ne_halted {P : Program} {H : Store} {φ : Frame} {K : List Kont}
    {tr : List Event} {vs : List Val} {t : ArgsTag} : stepArgs P H φ K tr vs t ≠ .halted := by
  intro h
  cases t <;> simp only [stepArgs] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `stepRet` never answers `halted` (helper). -/
theorem stepRet_ne_halted {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {v : Val} {k : Kont} : stepRet M P H φ K tr v k ≠ .halted := by
  intro h
  cases k <;> simp only [stepRet, OpRes.toStep] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `step` answers `halted` exactly at the terminal configurations: `✓n` and
`↯κ` (§6.12's (Result-Ok) and (Result-Panic)). -/
theorem step_halted_iff {M : FloatOps} {P : Program} {C : Config} :
    step M P C = .halted ↔ C.Terminal := by
  match C with
  | .panic _ _ => simp [step, Config.Terminal]
  | .run H φ K (.eval e) tr => simp only [step, Config.Terminal, iff_false]; exact stepEval_ne_halted
  | .run H φ K (.args t vs (e :: es)) tr => simp [step, Config.Terminal]
  | .run H φ K (.args t vs []) tr =>
      simp only [step, Config.Terminal, iff_false]; exact stepArgs_ne_halted
  | .run _ _ [] (.ret _) _ => simp [step, Config.Terminal]
  | .run H φ (k :: K) (.ret v) tr =>
      simp only [step, Config.Terminal, iff_false]; exact stepRet_ne_halted

/-- **A stuck configuration** (§6, §7): not terminal, and no rule of §6
applies. `step` names the reason with the `Violation` the interpreter uses for
the same configuration. -/
def Config.Stuck (M : FloatOps) (P : Program) (C : Config) (w : Violation) : Prop :=
  step M P C = .stuck w

/-- **Every configuration is terminal, steps, or is stuck** (§6, §7's
phrasing of progress): the three cases are exclusive (`step` is a function)
and exhaustive, and a stuck one is named. -/
theorem Config.trichotomy (M : FloatOps) (P : Program) (C : Config) :
    (∃ C', Step M P C C') ∨ C.Terminal ∨ ∃ w, C.Stuck M P w := by
  cases h : step M P C with
  | next C' => exact .inl ⟨C', step_iff.mpr h⟩
  | halted => exact .inr (.inl (step_halted_iff.mp h))
  | stuck w => exact .inr (.inr ⟨w, h⟩)

/-- A stuck configuration takes no step (§6). -/
theorem Config.Stuck.no_step {M : FloatOps} {P : Program} {C C' : Config} {w : Violation}
    (h : C.Stuck M P w) : ¬ Step M P C C' := by
  intro hs
  have := hs.step_eq
  simp [Config.Stuck] at h
  rw [h] at this
  cases this

/-! ## Stuck states are §6's, and the monitors are absent (RUE-2314) -/

/-- Whether a violation is one of **§6's own stuck states** — a read of a `⊘`
or `†` cell, an unbound index, a wrong-shaped operand — rather than one of the
three monitors `eval` adds, which §6.3, §6.7 and §6.8 do not have. -/
def Violation.isStuckState : Violation → Bool
  | .useAfterMove | .useAfterDrop | .unbound | .typeConfusion => true
  | .linearLeak | .linearOverwrite | .linearDiscard => false

/-- `readAt` refuses only with §6's stuck states (helper). -/
theorem Contents.readAt_err : ∀ {c : Contents} {π : List Nat} {w : Violation},
    c.readAt π = .error w → w.isStuckState = true
  | _, [], _, h => by simp [Contents.readAt] at h
  | .hole, _ :: _, _, h => by simp [Contents.readAt] at h; subst h; rfl
  | .struct _ cs, f :: π, _, h => by
      simp only [Contents.readAt] at h
      split at h
      · exact Contents.readAt_err h
      · simp at h; subst h; rfl
  | .array _ cs, f :: π, _, h => by
      simp only [Contents.readAt] at h
      split at h
      · exact Contents.readAt_err h
      · simp at h; subst h; rfl
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _, _ :: _, _, h => by
      simp [Contents.readAt] at h; subst h; rfl

mutual
/-- `split` refuses only with §6's stuck states (helper). -/
theorem Contents.splitResidue_err (D : Decls) : ∀ {c : Contents} {π : List Nat} {w : Violation},
    c.splitResidue D π = .error w → w.isStuckState = true
  | _, [], _, h => by simp [Contents.splitResidue] at h
  | .struct _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_err D h
  | .array _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_err D h
  | .hole, _ :: _, _, h => by simp [Contents.splitResidue] at h; subst h; rfl
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _, _ :: _, _, h => by
      simp [Contents.splitResidue] at h; subst h; rfl

/-- `split`'s field step refuses only with §6's stuck states (helper). -/
theorem Contents.splitFields_err (D : Decls) :
    ∀ {cs : List Contents} {f : Nat} {π : List Nat} {w : Violation},
    Contents.splitFields D cs f π = .error w → w.isStuckState = true
  | [], _, _, _, h => by simp [Contents.splitFields] at h; subst h; rfl
  | c :: _, 0, π, _, h => by
      simp only [Contents.splitFields] at h
      split at h
      · simp at h; subst h; exact Contents.splitResidue_err D ‹_›
      · simp at h
  | _ :: cs, f + 1, π, _, h => by
      simp only [Contents.splitFields] at h
      split at h
      · simp at h; subst h; exact Contents.splitFields_err D ‹_›
      · simp at h
end

mutual
/-- §6.11's `drop` refuses only with §6's stuck states (helper). -/
theorem dropContents_err (D : Decls) : ∀ {c : Contents} {w : Violation},
    dropContents D c = .error w → w.isStuckState = true
  | .hole, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
      simp [dropContents] at h
  | .struct s cs, _, h => by
      simp only [dropContents] at h
      split at h
      · simp at h; subst h; rfl
      · split at h
        · simp at h; subst h; exact dropContentsList_err D ‹_›
        · simp at h
  | .enum _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_err D h
  | .array _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_err D h

/-- `drop*` refuses only with §6's stuck states (helper). -/
theorem dropContentsList_err (D : Decls) : ∀ {cs : List Contents} {w : Violation},
    dropContentsList D cs = .error w → w.isStuckState = true
  | [], _, h => by simp [dropContentsList] at h
  | c :: cs, _, h => by
      simp only [dropContentsList] at h
      split at h
      · simp at h; subst h; exact dropContents_err D ‹_›
      · split at h
        · simp at h; subst h; exact dropContentsList_err D ‹_›
        · simp at h
end

/-- A binding's drop refuses only with §6's stuck states (helper). -/
theorem dropCell_err {D : Decls} {ℓ : Nat} {c : Contents} {w : Violation}
    (h : dropCell D ℓ c = .error w) : w.isStuckState = true := by
  simp only [dropCell] at h
  split at h
  · simp at h
  · split at h
    · simp at h; subst h; exact dropContents_err D ‹_›
    · simp at h

/-- `plainUnwind` refuses only with §6's stuck states (helper). -/
theorem plainUnwind_err {D : Decls} : ∀ {H : Store} {ls : List Nat} {w : Violation},
    plainUnwind D H ls = .error w → w.isStuckState = true
  | _, [], _, h => by simp [plainUnwind] at h
  | H, ℓ :: rest, _, h => by
      simp only [plainUnwind, plainDropRetire] at h
      split at h
      · rename_i heq
        split at heq
        · simp at heq; subst heq; simp at h; subst h; rfl
        · simp at heq; subst heq; simp at h; subst h; rfl
        · split at heq
          · simp at heq; subst heq; simp at h; subst h; exact dropCell_err ‹_›
          · simp at heq
      · split at h
        · simp at h; subst h; exact plainUnwind_err ‹_›
        · simp at h

/-- `plainDestructure` refuses only with §6's stuck states (helper). -/
theorem plainDestructure_err {D : Decls} {c : Contents} {πs : List Nat} {w : Violation}
    (h : plainDestructure D c πs = .error w) : w.isStuckState = true := by
  simp only [plainDestructure] at h
  split at h
  · simp at h; subst h; exact Contents.splitResidue_err D ‹_›
  · split at h
    · simp at h; subst h; exact dropContentsList_err D ‹_›
    · simp at h

/-- `rootCell` refuses only with §6's stuck states (helper). -/
theorem rootCell_err {H : Store} {φ : Frame} {i : Nat} {w : Violation}
    (h : rootCell H φ i = .error w) : w.isStuckState = true := by
  simp only [rootCell] at h
  repeat' split at h
  all_goals simp at h
  all_goals (subst h; rfl)

/-- Resolving a dynamic tail refuses only with §6's stuck states (helper). -/
theorem Contents.resolveDyn_err : ∀ {c : Contents} {is : List Int} {πs : List (List Nat)}
    {w : Violation}, c.resolveDyn is πs = .stuck w → w.isStuckState = true
  | c, [], [], _, h => by cases c <;> simp [Contents.resolveDyn] at h
  | c, i :: is, π :: πs, w, h => by
      cases c
      case array T cs =>
        simp only [Contents.resolveDyn] at h
        split at h
        · split at h
          · simp at h; subst h; rfl
          · split at h
            · simp at h; subst h; exact Contents.readAt_err ‹_›
            · split at h
              · simp at h
              · exact Contents.resolveDyn_err h
        · simp at h
      all_goals (simp [Contents.resolveDyn] at h; subst h; rfl)
  | c, [], _ :: _, _, h | c, _ :: _, [], _, h => by
      cases c <;> simp [Contents.resolveDyn] at h <;> (subst h; rfl)

/-- Navigating a dynamic place refuses only with §6's stuck states (helper). -/
theorem dynPlace_err {H : Store} {φ : Frame} {p : Place} {vs : List Val}
    {πs : List (List Nat)} {w : Violation}
    (h : dynPlace H φ p vs πs = .stuck w) : w.isStuckState = true := by
  simp only [dynPlace] at h
  repeat' split at h
  all_goals simp at h
  all_goals (try (subst h; rfl))
  · subst h; exact Contents.readAt_err ‹_›
  · subst h; exact Contents.resolveDyn_err ‹_›

/-- **§6's stuck states only** (RUE-2314): a configuration `step` finds stuck
is stuck on `useAfterMove`, `useAfterDrop`, `unbound` or `typeConfusion` —
never on `linearLeak`, `linearOverwrite` or `linearDiscard`, the three
monitors `eval` adds and §6.3, §6.7 and §6.8 do not have. -/
theorem step_stuck_isStuckState {M : FloatOps} {P : Program} {C : Config} {w : Violation}
    (h : C.Stuck M P w) : w.isStuckState = true := by
  simp only [Config.Stuck] at h
  match C, h with
  | .panic _ _, h => simp [step] at h
  | .run _ _ [] (.ret _) _, h => simp [step] at h
  | .run H φ K (.args t vs (e :: es)) tr, h => simp [step] at h
  | .run H φ K (.eval e) tr, h =>
      simp only [step] at h
      cases e <;> simp only [stepEval] at h
      all_goals (repeat' split at h)
      all_goals simp at h
      all_goals (try (subst h; rfl))
      all_goals subst h
      all_goals first
        | exact rootCell_err ‹_›
        | exact Contents.readAt_err ‹_›
        | exact plainDestructure_err ‹_›
        | exact dropCell_err ‹_›
        | exact plainUnwind_err ‹_›
  | .run H φ K (.args t vs []) tr, h =>
      simp only [step] at h
      cases t <;> simp only [stepArgs] at h
      all_goals (repeat' split at h)
      all_goals simp at h
      all_goals (try (subst h; rfl))
      all_goals subst h
      all_goals first
        | exact dynPlace_err ‹_›
        | exact Contents.readAt_err ‹_›
        | exact dropCell_err ‹_›
  | .run H φ (k :: K) (.ret v) tr, h =>
      simp only [step] at h
      cases k <;> simp only [stepRet, OpRes.toStep] at h
      all_goals (repeat' split at h)
      all_goals simp at h
      all_goals (try (subst h; rfl))
      all_goals subst h
      all_goals first
        | exact rootCell_err ‹_›
        | exact Contents.readAt_err ‹_›
        | exact dropCell_err ‹_›
        | exact dropContents_err _ ‹_›
        | exact plainUnwind_err ‹_›

/-! ## Where a monitor passes, the plain drop agrees -/

/-- Where `eval`'s leak monitor lets a scope exit through, §6's monitor-free
drop-retire does the same thing (§6.1's `drop-retire`) (helper). -/
theorem dropRetire_plain {D : Decls} {H : Store} {ℓ : Nat} {r : Store × List Event}
    (h : dropRetire D H ℓ = .ok r) : plainDropRetire D H ℓ = .ok r := by
  simp only [dropRetire] at h
  simp only [plainDropRetire]
  split at h
  · simp at h
  · simp at h
  · rename_i heq
    rw [heq]
    split at h
    · simp at h
    · exact h

/-- **The leak monitor only removes behaviour** (RUE-2314): where
`unwindLocs` — `run-scope-drops` with `eval`'s monitor — succeeds, §6's
monitor-free `plainUnwind` succeeds with the same store and trace. Parts 2
and 3 of the adequacy proof read every scope exit through this. -/
theorem unwindLocs_plain {D : Decls} : ∀ {H : Store} {ls : List Nat} {r : Store × List Event},
    unwindLocs D H ls = .ok r → plainUnwind D H ls = .ok r
  | _, [], _, h => by simpa [unwindLocs, plainUnwind] using h
  | H, ℓ :: rest, r, h => by
      simp only [unwindLocs] at h
      simp only [plainUnwind]
      split at h
      · simp at h
      · rename_i H₁ evs heq
        rw [dropRetire_plain heq]
        split at h
        · simp at h
        · rename_i heq'
          simp only [unwindLocs_plain heq']
          exact h

/-- The residue monitor passes only where `drop*` of the residue succeeds
with the same trace (helper). -/
theorem dropResidue_plain {D : Decls} : ∀ {rs : List Contents} {evs : List Event},
    dropResidue D rs = .ok evs → dropContentsList D rs = .ok evs
  | [], _, h => by simpa [dropResidue, dropContentsList] using h
  | r :: rs, evs, h => by
      simp only [dropResidue] at h
      simp only [dropContentsList]
      split at h
      · simp at h
      · split at h
        · simp at h
        · rename_i heq
          try rw [heq]
          split at h
          · simp at h
          · rename_i heq'
            simp only [dropResidue_plain heq']
            exact h

/-- **The residue monitor only removes behaviour** (RUE-2314): where
`eval`'s monitored `destructure` succeeds, §6.3's monitor-free
`destructure` succeeds with the same leaf and trace. -/
theorem destructure_plain {D : Decls} {c : Contents} {πs : List Nat}
    {r : Contents × List Event} (h : c.destructure D πs = .ok r) :
    plainDestructure D c πs = .ok r := by
  simp only [Contents.destructure] at h
  simp only [plainDestructure]
  split at h
  · simp at h
  · rename_i leaf rs heq
    rw [heq]
    simp only
    split at h
    · simp at h
    · rename_i heq'
      rw [dropResidue_plain heq']
      exact h

/-! ## Running the relation -/

/-- Take up to `n` steps of `step`, stopping early at a configuration that
takes none (helper). -/
def stepN (M : FloatOps) (P : Program) : Nat → Config → Config
  | 0, C => C
  | n + 1, C =>
    match step M P C with
    | .next C' => stepN M P n C'
    | .halted | .stuck _ => C

/-- Whatever `stepN` reaches, `→*` reaches (§6.12's `→*`), so a run of the
function is a derivation of the relation (helper). -/
theorem stepN_steps {M : FloatOps} {P : Program} : ∀ {n : Nat} {C : Config},
    Steps M P C (stepN M P n C)
  | 0, C => .refl C
  | n + 1, C => by
      simp only [stepN]
      split
      · rename_i C' h
        exact .step (step_iff.mpr h) stepN_steps
      · exact .refl C
      · exact .refl C

/-- A two-line program, `let x = 40; x + 2`, as the entry point returning
`i32` (helper). -/
def letAddProgram : Program :=
  Program.entry (Decls.ofStructs []) (.int .w32 .signed)
    (.letIn false (.intLit .w32 .signed 40) (.binop .add (.use (.var 0)) (.intLit .w32 .signed 2)))

/-- **The relation runs a program to the same answer `eval` does**, a check
the two presentations can be compared on before the adequacy theorems say
they always agree: from §6.12's initial configuration, `→*` reaches `✓42`
through (D-Call), (D-Let), (D-Use-Copy), (D-Arith), (D-EndScope) and
(D-Return-Value), with the `let`'s cell retired and nothing printed; and
`run` answers the same value, store and trace. -/
theorem letAddProgram_runs (M : FloatOps) :
    Steps M letAddProgram Config.init
      (.run [.dead] { env := [], scope := [] } [] (.ret (.int .w32 .signed 42)) []) ∧
    run M letAddProgram 100 = .ok [.dead] (.int .w32 .signed 42) [] :=
  ⟨stepN_steps (n := 30), rfl⟩

end RueCore
