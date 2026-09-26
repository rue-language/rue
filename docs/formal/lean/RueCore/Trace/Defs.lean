module

public import RueCore.Step
public import RueCore.Soundness.Defs

@[expose] public section

/-!
# RueCore.Trace.Defs — what the trace theorems are stated over (layer L1)

The definitions §7's trace theorems write their statements in: owned
identities and the trace's projections (`freedIds`, `dtorIds`, from
`Trace.lean`), the conservation law's and the exact ledger's per-evaluation
promises (`Cons`, `Exact`, `Lead`, `Tidy`, `Settled`), the RUE-2316 carve-out
(`Program.pendingSafe`, from `TraceExact.lean`), and the drop order's grammar
and the configuration invariants over §6's relation (`Blocks`,
`Config.Ordered`, `Config.Nested`, `NewestFirst`, `Lifo`, from
`TraceOrder.lean`).

They are moved here verbatim (RUE-2456), in the order the three proof modules
declared them, so that the statements' vocabulary sits in the definitions
layer, below every proof; the audit `lake exe ruecore-layers` keeps it that
way (README, "Layers"). Each definition's docstring still cites the calculus;
the lemmas about them stay with the proofs.
-/

namespace RueCore

mutual
/-- The identities of a contents' **owned** nodes: every non-`Copy` aggregate
in it, `⊘` skipped (§6.11's skip) and nothing below a `Copy` node, since a
`Copy` value is duplicated freely and has no drop glue. -/
def Contents.own (D : Decls) : Contents → List Nat
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => []
  | .struct s i cs => if D.classOf s = .copy then [] else i :: Contents.ownList D cs
  | .enum e _ i cs => if D.enumClassOf e = .copy then [] else i :: Contents.ownList D cs
  | .array T i cs =>
      if Ty.mult D (.array T cs.length) = .copy then [] else i :: Contents.ownList D cs

/-- `own` over a field, payload or element list (helper). -/
def Contents.ownList (D : Decls) : List Contents → List Nat
  | [] => []
  | c :: cs => Contents.own D c ++ Contents.ownList D cs
end

/-- A value's owned identities: its stored image's (helper). -/
abbrev Val.own (D : Decls) (v : Val) : List Nat := (Contents.ofVal v).own D

/-- A cell's owned identities: a retired cell, or a reserved identity slot,
holds none (helper). -/
def Cell.own (D : Decls) : Cell → List Nat
  | .full c => c.own D
  | .dead => []

/-- The store's owned identities, cell by cell (helper). -/
def storeOwn (D : Decls) (H : Store) : List Nat := H.flatMap (Cell.own D)

/-- Every live cell of the store is copy-closed (helper). -/
def StoreCC (D : Decls) (H : Store) : Prop :=
  ∀ (ℓ : Nat) (c : Contents), H[ℓ]? = some (Cell.full c) → c.copyClosed D = true

/-- Multiset inclusion, read by counts: every identity occurs in `l₁` at most
as often as in `l₂` (helper). -/
def IdLe (l₁ l₂ : List Nat) : Prop := ∀ a, l₁.count a ≤ l₂.count a

/-- The identities minted between two stores: the indices the store grew by,
since `introVal` takes the next index (helper). -/
def Fresh (H H' : Store) : List Nat := List.range' H.length (H'.length - H.length)

/-- The owned identities a `drop` or `dropTemp` marker frees: the whole tree
§6.11's walk goes through — and the shell a `consume` event ends (RUE-2427),
whose members were already moved out or dropped. A destructor event frees
nothing of its own — it is nested under a marker — and a `@dbg` frees nothing
(helper). -/
def Event.freed (D : Decls) : Event → List Nat
  | .drop _ c => c.own D
  | .dropTemp v => v.own D
  | .consume c => c.own D
  | .dtor _ _ | .dbg _ => []

/-- The identity of the value a user destructor ran on (§6.11, `3.9:28`) —
every `dtor` event carries the struct it ran on (helper). -/
def Event.dtorIds : Event → List Nat
  | .dtor _ (.struct _ i _) => [i]
  | _ => []

/-- The identities the trace's markers free, in trace order: what §7's
no-double-free bullet counts at a drop. -/
def freedIds (D : Decls) (tr : List Event) : List Nat := tr.flatMap (Event.freed D)

/-- The identities the trace's destructors ran on, in trace order: what §7's
no-double-free bullet counts at a destructor (`3.9:28`). -/
def dtorIds (tr : List Event) : List Nat := tr.flatMap Event.dtorIds

/-- The trace a result carries: everything the run emitted, for a value, an
unwinding `return` or `break`, and a trap; nothing for a refusal or exhausted
fuel (helper). -/
def EvalRes.trace : EvalRes → List Event
  | .ok _ _ tr | .returned _ _ tr | .broke _ _ tr | .panic _ tr => tr
  | .stuck _ | .outOfFuel => []

/-- A destructor-bearing struct is not `Copy` (`3.9:31`): the one fact about
the declarations the conservation law reads (helper). -/
def DtorNotCopy (D : Decls) : Prop :=
  ∀ (s : Nat) (sd : StructDecl), D.structs[s]? = some sd → sd.dtor = true → D.classOf s ≠ .copy

/-- What the conservation law asks of a projection `F` of the trace onto
identities: §6.11's walk projects to at most what the dropped contents owns,
with a binding's `drop` marker or a temporary's `dropTemp` marker on top, and
a `@dbg` projects to nothing. `freedIds` and `dtorIds` are the two projections
§7's bullet is about (`freed_measure`, `dtor_measure`) (helper). -/
structure TraceMeasure (D : Decls) (F : Event → List Nat) : Prop where
  /-- §6.11's walk, with no marker: a destructure's `Copy` residue subtree
  (§6.3), which `residueMark` gives no marker. -/
  walk : ∀ {c : Contents} {evs : List Event}, c.copyClosed D = true →
    dropContents D c = .ok evs → IdLe (evs.flatMap F) (c.own D)
  /-- A binding's drop: its `drop ℓ c` marker, then the walk (§6.11). -/
  marker : ∀ {ℓ : Nat} {c : Contents} {evs : List Event}, c.copyClosed D = true →
    dropContents D c = .ok evs → IdLe (F (.drop ℓ c) ++ evs.flatMap F) (c.own D)
  /-- A discarded temporary: its `dropTemp v` marker, then the walk (§6.7). -/
  temp : ∀ {v : Val} {evs : List Event}, (Contents.ofVal v).copyClosed D = true →
    dropContents D (Contents.ofVal v) = .ok evs → IdLe (F (.dropTemp v) ++ evs.flatMap F) (v.own D)
  /-- A consumption ends at most its shell (RUE-2427). -/
  consume : ∀ c, IdLe (F (.consume c)) (c.own D)
  /-- `@dbg` frees nothing. -/
  dbg : ∀ v, F (.dbg v) = []

/-- **The conservation law, for one evaluation** from store `H`, holding the
owned identities `X` besides it (a pending operand's value): the result's
store, its value and the trace's projection `F` together own at most what `H`
and `X` owned, plus what was minted on the way — each identity counted, as a
multiset. The store and the value stay copy-closed. A trap carries no store,
so its minted range is existential; a refusal and exhausted fuel promise
nothing (helper). -/
def Cons (D : Decls) (F : Event → List Nat) (H : Store) (X : List Nat) : EvalRes → Prop
  | .ok H' v tr | .returned H' v tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧ (Contents.ofVal v).copyClosed D = true ∧
      ∀ a, (storeOwn D H').count a + (v.own D).count a + (tr.flatMap F).count a
        ≤ (storeOwn D H).count a + X.count a + (Fresh H H').count a
  | .broke H' _ tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧
      ∀ a, (storeOwn D H').count a + (tr.flatMap F).count a
        ≤ (storeOwn D H).count a + X.count a + (Fresh H H').count a
  | .panic _ tr =>
      ∃ N, ∀ a, (tr.flatMap F).count a
        ≤ (storeOwn D H).count a + X.count a + (List.range' H.length N).count a
  | .stuck _ | .outOfFuel => True

mutual
/-- Whether an expression contains a `return` anywhere — including under a
loop (helper). -/
def Expr.returns : Expr → Bool
  | .ret _ => true
  | .brk => false
  | .intLit _ _ _ | .floatLit _ _ | .boolLit _ | .unitLit | .use _ | .panic _
  | .drop _ => false
  | .binop _ e₁ e₂ | .letIn _ e₁ e₂ | .seq e₁ e₂ => e₁.returns || e₂.returns
  | .unop _ e | .intCast _ _ e | .fintrin _ e | .dbg e | .repeatArray _ e _
  | .assign _ e | .loop e => e.returns
  | .mkStruct _ args | .mkEnum _ _ args | .mkArray _ args | .call _ args
  | .indexRead _ args _ | .indexDrop _ args _ => Expr.returnsList args
  | .indexWrite _ idx _ e => e.returns || Expr.returnsList idx
  | .ite c e₁ e₂ => c.returns || e₁.returns || e₂.returns
  | .«match» scrut arms => scrut.returns || Expr.returnsList arms

/-- `Expr.returns` over a list (helper). -/
def Expr.returnsList : List Expr → Bool
  | [] => false
  | e :: es => e.returns || Expr.returnsList es
end

/-- Whether evaluating an expression can **unwind** past its context: it
contains a `return`, or a `break` its own loops do not catch
(`Expr.breaks`) (helper). -/
def Expr.unwinds (e : Expr) : Bool := e.returns || e.breaks

/-- No expression of the list unwinds (helper). -/
def Expr.quietList (es : List Expr) : Bool := es.all fun e => !e.unwinds

mutual
/-- **The RUE-2316 carve-out, syntactically**: no value computed for one
operand is pending while a later operand of the same form can unwind — a
call's arguments, a struct, enum or array literal's members, an index list,
a binary operator's two operands, and an indexed assignment's right-hand side
before its indices (`5.2:14`). The first operand may unwind: nothing is
pending yet. `binop` is in the list although RUE-2316's text does not name it:
its left operand is a scalar under (Arith) §5.8, so nothing owned is lost
there on a checked program, but the carve-out is syntactic and cannot see the
type. -/
def Expr.pendingSafe : Expr → Bool
  | .intLit _ _ _ | .floatLit _ _ | .boolLit _ | .unitLit | .use _ | .panic _
  | .drop _ | .brk => true
  | .binop _ e₁ e₂ => e₁.pendingSafe && e₂.pendingSafe && !e₂.unwinds
  | .unop _ e | .intCast _ _ e | .fintrin _ e | .dbg e | .repeatArray _ e _
  | .assign _ e | .ret e | .loop e => e.pendingSafe
  | .mkStruct _ args | .mkEnum _ _ args | .mkArray _ args | .call _ args
  | .indexRead _ args _ | .indexDrop _ args _ =>
      Expr.pendingSafeList args && Expr.quietList args.tail
  | .indexWrite _ idx _ e => e.pendingSafe && Expr.pendingSafeList idx && Expr.quietList idx
  | .letIn _ e₁ e₂ | .seq e₁ e₂ => e₁.pendingSafe && e₂.pendingSafe
  | .ite c e₁ e₂ => c.pendingSafe && e₁.pendingSafe && e₂.pendingSafe
  | .«match» scrut arms => scrut.pendingSafe && Expr.pendingSafeList arms

/-- `Expr.pendingSafe` over a list (helper). -/
def Expr.pendingSafeList : List Expr → Bool
  | [] => true
  | e :: es => e.pendingSafe && Expr.pendingSafeList es
end

/-- Every function body of the program is `pendingSafe` (helper). -/
def Program.pendingSafe (P : Program) : Bool := P.fns.all fun fd => fd.body.pendingSafe

/-- **The exact ledger for one evaluation** from store `H`, holding the owned
identities `X` besides it (a pending operand's value): for every identity the
evaluation **starts** with (`a < H.length`), the result's store, its value
and the identities the trace ends together hold it exactly as often as `H` and
`X` did — it is still in the store, in the result, or ended once in the trace,
and nothing is lost or duplicated. Identities minted during the evaluation
(`≥ H.length`) are not counted; `no_double_free` bounds those. A trap, a
refusal and exhausted fuel promise nothing (helper). -/
def Exact (D : Decls) (H : Store) (X : List Nat) : EvalRes → Prop
  | .ok H' v tr | .returned H' v tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧ (Contents.ofVal v).copyClosed D = true ∧
      ∀ a, a < H.length → (storeOwn D H').count a + (v.own D).count a + (freedIds D tr).count a
        = (storeOwn D H).count a + X.count a
  | .broke H' _ tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧
      ∀ a, a < H.length → (storeOwn D H').count a + (freedIds D tr).count a
        = (storeOwn D H).count a + X.count a
  | .panic _ _ | .stuck _ | .outOfFuel => True

/-- **A form's leading operands have run** (helper): from store `H` in frame
`φ` at fuel `fuel`, the form's first operand — or its argument list, for a
call, a literal and a dynamic read — produced the values `vs` in store `H₁`,
after trace `tr`. A `@drop` below a dynamic index runs the read first. A
`loop`'s lead is its body **breaking**: its rest is (D-Break)'s unwind, which
ends the values the body still held — the cells the carried record owes, the
body's own bindings included (§6.10). -/
def Lead (M : FloatOps) (P : Program) (fuel : Nat) (H : Store) (φ : Frame) (H₁ : Store)
    (vs : List Val) (tr : List Event) : Expr → Prop
  | .letIn _ e₁ _ | .seq e₁ _ | .«match» e₁ _ | .assign _ e₁ | .ret e₁ | .dbg e₁
  | .repeatArray _ e₁ _ | .indexWrite _ _ _ e₁ | .binop _ e₁ _ | .unop _ e₁
  | .intCast _ _ e₁ | .fintrin _ e₁ | .ite e₁ _ _ =>
      ∃ v, vs = [v] ∧ eval M fuel P H φ e₁ = .ok H₁ v tr
  | .indexDrop p idx πs => ∃ v, vs = [v] ∧ eval M fuel P H φ (.indexRead p idx πs) = .ok H₁ v tr
  | .call _ args | .mkStruct _ args | .mkEnum _ _ args | .mkArray _ args | .indexRead _ args _ =>
      evalArgs (fun H' e => eval M fuel P H' φ e) H args = .ok H₁ vs tr
  | .loop e₁ => ∃ sc, vs = [] ∧ eval M fuel P H φ e₁ = .broke H₁ sc tr
  | .intLit _ _ _ | .floatLit _ _ | .boolLit _ | .unitLit | .use _ | .panic _ | .drop _
  | .brk => False

/-- The store only grew, and a cell outside the frame's environment was left
alone or retired (helper). -/
def Local (φ : Frame) (H H' : Store) : Prop :=
  H.length ≤ H'.length ∧
    ∀ ℓ, ℓ < H.length → ℓ ∉ φ.env → H'[ℓ]? = H[ℓ]? ∨ H'[ℓ]? = some .dead

/-- Every cell allocated since `H` is retired, but those `keep` names
(helper). -/
def Retired (H : Store) (keep : List Nat) (H' : Store) : Prop :=
  ∀ ℓ, H.length ≤ ℓ → ℓ < H'.length → ℓ ∉ keep → H'[ℓ]? = some .dead

/-- **The frame-pop invariant for one evaluation** in frame `φ` from store `H`
(§6.7, §6.9, §6.10): the store only grew and was touched outside `φ`'s
environment only to retire; every cell the evaluation allocated is retired by
its end — for an unwinding `break`, all but the cells of the scope record it
carries, which extends `φ`'s by cells allocated since `H` and which the loop
retires; and an unwinding
`return` has retired every cell of `φ`'s record (§6.9's σ-walk). -/
def Tidy (φ : Frame) (H : Store) : EvalRes → Prop
  | .ok H' _ _ => Local φ H H' ∧ Retired H [] H'
  | .returned H' _ _ => Local φ H H' ∧ Retired H [] H' ∧ ∀ ℓ ∈ φ.scope, H'[ℓ]? = some .dead
  | .broke H' sc _ => Local φ H H' ∧ Retired H sc H' ∧
      ∃ locs, sc = φ.scope ++ locs ∧ ∀ ℓ ∈ locs, H.length ≤ ℓ
  | .panic _ _ | .stuck _ | .outOfFuel => True

/-- What the rest of a form owes the cells allocated after its leading
operands ran: every one retired by the form's end — but, for an unwinding
`break`, the ones its record owes the loop — and, for an unwinding `return`,
the frame's whole record retired (helper). -/
def Settled (φ : Frame) (H₁ : Store) : EvalRes → Prop
  | .ok H' _ _ => Retired H₁ [] H'
  | .returned H' _ _ => Retired H₁ [] H' ∧ ∀ ℓ ∈ φ.scope, H'[ℓ]? = some .dead
  | .broke H' sc _ => Retired H₁ sc H' ∧ ∃ locs, sc = φ.scope ++ locs
  | .panic _ _ | .stuck _ | .outOfFuel => True

/-- **§6.11's order, as a grammar over the trace.** A trace is a sequence of
blocks: a `@dbg` line, a consumption (`consume c`, which runs no drop of its
own), or a drop marker followed by exactly the events §6.11's walk of what it
names emits (`dropEvents`) — for a binding's drop `drop ℓ c`, the contents
`c`, and for a discarded temporary `dropTemp v`, the value `v`. A destructor
event (`dtor`) appears only inside such a walk, so the grammar says where
every destructor runs: inside the drop of the value that owns it, after the
destructors of everything dropped before it in §6.11's order (§3.9, §6.11). -/
inductive Blocks (D : Decls) : List Event → Prop
  | nil : Blocks D []
  | dbg {v : Val} {t : List Event} : Blocks D t → Blocks D (.dbg v :: t)
  | consume {c : Contents} {t : List Event} : Blocks D t → Blocks D (.consume c :: t)
  | drop {ℓ : Nat} {c : Contents} {t : List Event} :
      Blocks D t → Blocks D (.drop ℓ c :: (dropEvents D c ++ t))
  | dropTemp {v : Val} {t : List Event} :
      Blocks D t → Blocks D (.dropTemp v :: (dropEvents D (.ofVal v) ++ t))

mutual
/-- **§6.11's drop, rule by rule** (RUE-2487): `DropGlue D c evs` says that
dropping the cell contents `c` emits exactly the events `evs`. It is written
from §6.11's equations and `3.9`'s order, not from the machine, and mentions
neither `dropContents` nor `dropEvents`, so a change to the machine's drop glue
does not change it. One constructor per equation of §6.11:

* `drop(H, ⊘) = H`: a moved-out or uninitialised position emits nothing
  (`hole`), at every depth, which is `3.8:73`'s "elements that were moved out
  … are not dropped";
* `drop(H, n_T) = drop(H, f_T) = drop(H, b) = drop(H, ⟨⟩) = H`: a scalar emits
  nothing (`int`, `float`, `bool`, `unit`);
* `drop(H, { c1,…,ck }_S) = drop*(H, [c1,…,ck])` when `S` declares no
  destructor: the fields' drops in **declaration order** (`3.9:13`)
  (`struct`);
* the destructor case: when `S` declares `drop fn S(self)`, the destructor
  runs **first** (`3.9:28`), then the fields in declaration order
  (`structDtor`). The destructor is one `dtor` event: the fragment has no
  destructor bodies, and §6.11's residual fields are the original ones
  (`3.9:33`, `3.9:34`);
* `drop(H, [ c1,…,cn ]) = drop*(H, [c1,…,cn])`: the elements in **ascending
  index order** (`3.9:15`, `3.8:73`) (`array`);
* `drop(H, Kj⟨ c1,…,ca ⟩) = drop*(H, [c1,…,ca])`: only the **active**
  variant's payload (`6.3:20`) — an enum's contents hold that payload and no
  other — and an enum runs no destructor of its own (§3, E0417) (`enum`).

A struct's `k`-th member is its declaration's `k`-th field (`3.9:13`,
`StructDecl.fields`), so list order is declaration order. A struct index the
declarations do not have has no rule: §6.11 drops only declared types. -/
inductive DropGlue (D : Decls) : Contents → List Event → Prop
  | hole : DropGlue D .hole []
  | int {w : IntWidth} {s : Sign} {n : Int} : DropGlue D (.int w s n) []
  | float {w : FloatWidth} {f : FloatDatum} : DropGlue D (.float w f) []
  | bool {b : Bool} : DropGlue D (.bool b) []
  | unit : DropGlue D .unit []
  | struct {s i : Nat} {cs : List Contents} {sd : StructDecl} {evs : List Event} :
      D.structs[s]? = some sd → sd.dtor = false → DropGlueSeq D cs evs →
      DropGlue D (.struct s i cs) evs
  | structDtor {s i : Nat} {cs : List Contents} {sd : StructDecl} {evs : List Event} :
      D.structs[s]? = some sd → sd.dtor = true → DropGlueSeq D cs evs →
      DropGlue D (.struct s i cs) (.dtor s (.struct s i cs) :: evs)
  | array {T : Ty} {i : Nat} {cs : List Contents} {evs : List Event} :
      DropGlueSeq D cs evs → DropGlue D (.array T i cs) evs
  | enum {e k i : Nat} {cs : List Contents} {evs : List Event} :
      DropGlueSeq D cs evs → DropGlue D (.enum e k i cs) evs

/-- **§6.11's `drop*(H, [c1,…,cm])`**, which "folds `drop` over the list
left-to-right": the first member's drop, then the rest's (RUE-2487). -/
inductive DropGlueSeq (D : Decls) : List Contents → List Event → Prop
  | nil : DropGlueSeq D [] []
  | cons {c : Contents} {cs : List Contents} {e₁ e₂ : List Event} :
      DropGlue D c e₁ → DropGlueSeq D cs e₂ → DropGlueSeq D (c :: cs) (e₁ ++ e₂)
end

/-- **§6.11's order as a grammar over the trace, stated independently of the
machine** (RUE-2487). The same block grammar as `Blocks` — a `@dbg` line, a
consumption, or a drop marker followed by its drop's events — except that a
drop's events are given by §6.11's rules (`DropGlue`) rather than by the
function `dropEvents` the machine's walk is proved equal to. So a trace in
this grammar runs each value's destructor first, then its fields in
declaration order, an array's elements ascending and an enum's active payload
only, whatever the machine's own drop glue says. -/
inductive GlueBlocks (D : Decls) : List Event → Prop
  | nil : GlueBlocks D []
  | dbg {v : Val} {t : List Event} : GlueBlocks D t → GlueBlocks D (.dbg v :: t)
  | consume {c : Contents} {t : List Event} : GlueBlocks D t → GlueBlocks D (.consume c :: t)
  | drop {ℓ : Nat} {c : Contents} {evs t : List Event} :
      DropGlue D c evs → GlueBlocks D t → GlueBlocks D (.drop ℓ c :: (evs ++ t))
  | dropTemp {v : Val} {evs t : List Event} :
      DropGlue D (.ofVal v) evs → GlueBlocks D t → GlueBlocks D (.dropTemp v :: (evs ++ t))

/-- A scope record in **registration order is location order**: its cells
strictly increasing, every one below the store's length `n` (helper). -/
def Rec (n : Nat) (ls : List Nat) : Prop := ls.Pairwise (· < ·) ∧ ∀ ℓ ∈ ls, ℓ < n

/-- What a frame of the control stack owes, ordered: a pending `endscope`
marker's cells, and the scope record of a suspended caller (`ret(E, φ)`) or
of a loop boundary (`loopβ(e, φ)`) (helper). -/
def Kont.Ordered (n : Nat) : Kont → Prop
  | .endscope ls => Rec n ls
  | .loop _ φ => Rec n φ.scope
  | .call φ => Rec n φ.scope
  | _ => True

/-- **Every scope record of a configuration is in registration order**, which
is location order: the current frame's, and every one the control stack
holds (§6.1's `σ`, §6.7's `endscope`, §6.9's `ret(E, φ)`, §6.10's
`loopβ(e, φ)`). -/
def Config.Ordered : Config → Prop
  | .run H φ K _ _ => Rec H.length φ.scope ∧ ∀ k ∈ K, k.Ordered H.length
  | .panic _ _ => True

/-- The cells a trace's `drop` markers name, in trace order (helper). -/
def dropLocs (tr : List Event) : List Nat :=
  tr.filterMap fun | .drop ℓ _ => some ℓ | _ => none

/-- **The order one step drops cells in**: its `drop` markers either all name
one cell — an overwrite, an `@drop`, a destructure's residue, several
sub-positions of one binding — or name distinct cells in strictly decreasing
location order (helper). -/
def NewestFirst (ls : List Nat) : Prop := (∃ ℓ, ∀ x ∈ ls, x = ℓ) ∨ ls.Pairwise (· > ·)

/-- The output a configuration has produced so far (§6.12) (helper). -/
def Config.trace : Config → List Event
  | .run _ _ _ _ tr => tr
  | .panic _ tr => tr

/-- The scope record the pending `endscope` markers and loop boundaries of
one frame account for (§6.7, §6.10): reading the stack top-down, each
`endscope ℓs` is the tail of what is left of the record, a loop boundary
`loopβ(e, φs)` has exactly `φs`'s record left, and a caller's frame
`ret(E, φs)` starts the same reading over for the caller's record `φs`
(helper). -/
def Nest : List Nat → List Kont → Prop
  | _, [] => True
  | sc, .endscope ls :: K => ∃ sc', sc = sc' ++ ls ∧ Nest sc' K
  | sc, .loop _ φs :: K => sc = φs.scope ∧ Nest φs.scope K
  | _, .call φs :: K => Nest φs.scope K
  | sc, _ :: K => Nest sc K

/-- The registration records of the suspended callers, bottom of the stack
first (§6.9's `ret(E, φ)` frames) (helper). -/
def Stk : List Kont → List Nat
  | [] => []
  | .call φs :: K => Stk K ++ φs.scope
  | _ :: K => Stk K

/-- The machine's whole **registration stack**: every suspended caller's
scope record, bottom first, then the current frame's (§6.1's `σ` per frame);
empty at a trap (helper). -/
def Config.stack : Config → List Nat
  | .run _ φ K _ _ => Stk K ++ φ.scope
  | .panic _ _ => []

/-- **Scopes nest** (§6.7, §6.9, §6.10): every frame's pending `endscope`
markers are exactly the tail of its scope record, innermost last, and the
whole registration stack is in location order, below the store's length. -/
def Config.Nested : Config → Prop
  | .run H φ K _ _ => Nest φ.scope K ∧ Rec H.length (Stk K ++ φ.scope)
  | .panic _ _ => True

/-- **How one step changes the registration stack, and what it drops**: it
either keeps the stack as a prefix of the new one — nothing deregistered —
or cuts the stack back to a prefix of the old one, and its `drop` markers
then name only cells of the suffix it cut, newest first (helper). -/
def Lifo (S S' : List Nat) (ls : List Nat) : Prop :=
  S <+: S' ∨ (S' <+: S ∧ ls.Sublist (S.drop S'.length).reverse)

/-- The owned identities an argument list's tag holds: an indexed
assignment's right-hand side, already a value while its indices are reduced
(`5.2:14`); no other tag holds a value (helper). -/
def ArgsTag.own (D : Decls) : ArgsTag → List Nat
  | .indexWrite _ _ v => v.own D
  | _ => []

/-- The owned identities one control-stack frame holds (§6.1's `K`, §6.2's
`E`): a binary operator's left operand, reduced while the right one is, and a
list context's reduced values. A `call` or loop frame holds a scope record,
whose cells are in the store, and no other frame holds a value (helper). -/
def Kont.own (D : Decls) : Kont → List Nat
  | .binopR _ v => v.own D
  | .args t vs _ => t.own D ++ Contents.ownList D (Contents.ofVals vs)
  | _ => []

/-- The owned identities the focus holds: a value returned into the top
frame's hole, or a list context's reduced values (helper). -/
def Focus.own (D : Decls) : Focus → List Nat
  | .eval _ => []
  | .ret v => v.own D
  | .args t vs _ => t.own D ++ Contents.ownList D (Contents.ofVals vs)

/-- **What a configuration holds** (§6.1, RUE-2478): the owned identities of
its store's cells, of the value or values in focus, and of every value its
control stack holds pending — everywhere a running program keeps an owned
value. A trap holds nothing: §6.12's `↯κ` keeps a trace and no store. An
owned identity "allocated along a run" is one some configuration of the run
holds; (D-Struct), (D-Enum-Intro) and (D-Array) put a non-`Copy` aggregate's
fresh identity here the step they mint it. -/
def Config.held (D : Decls) : Config → List Nat
  | .run H _ K f _ => storeOwn D H ++ f.own D ++ K.flatMap (Kont.own D)
  | .panic _ _ => []

end RueCore
