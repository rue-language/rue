module

public import RueCore.Dynamics

@[expose] public section

/-!
# RueCore.Soundness.Defs — what `soundness` is stated over (layer L1)

The definitions §7's safety theorems (`Soundness.lean`) and the trace theorems
(`Trace.lean`, `TraceExact.lean`) write their statements in: §6.1's value and
contents typing (`HasTy`, `ContentsTy`), the store–context agreement the
invariant carries (`ContentsMatches`, `Matches`, `FrameMatches`), and the
promise `soundness` makes about `eval`'s result (`EvalOk`, `BrokeOk`).

They are moved here verbatim from `Soundness.lean` (RUE-2456) so that the
claims' vocabulary sits in the definitions layer, below every proof: this
module imports only `Dynamics`, and the audit `lake exe ruecore-layers` keeps
it that way (README, "Layers"). The lemmas about these definitions stay in
`Soundness.lean`.
-/

namespace RueCore

mutual
/-- Value typing: §6.1's values against §2's types, with the `n_T` bounds and,
for a struct value, its declaration's field list — §6.1's `{ v1, …, vk }_S`
well typed at `S` exactly when each field value is well typed at its declared
type (§5.8's (Struct-Intro), read on values). §7's preservation half is stated
over this relation. -/
inductive HasTy (D : Decls) : Val → Ty → Prop where
  | int {w s n} : InBounds w s n → HasTy D (.int w s n) (.int w s)
  /-- §6.1's `f_T` at `T = float(w)`: the datum lies in `𝔽_w`, which is the
  float counterpart of `n_T`'s `min_T ≤ n ≤ max_T` side condition. Keeping it
  is what gives §7's "totality of the float operations" lemma something to
  preserve: the model's closure laws (`FloatModel`, `Float.lean`) are exactly
  what re-establishes it after a rounded operation. -/
  | float {w f} : f.Wf w → HasTy D (.float w f) (.float w)
  | bool {b} : HasTy D (.bool b) .bool
  | unit : HasTy D .unit .unit
  | struct {s sd i vs} :
      D.structs[s]? = some sd → HasTys D vs sd.fields → HasTy D (.struct s i vs) (.struct s)
  /-- §6.1's `Kj⟨ v1, …, va ⟩` at `E`: the tag names a variant of the
  declaration — which is what progress at a `match` reads (`exhaustive_arm_exists`)
  — and the payload is well typed at that variant's declared component types
  ((Enum-Intro) §5.5, read on values). Nothing relates the value to the *other*
  variants: `class(E)` does (§3), and that is a fact about the type. -/
  | enum {e k ed Ts i vs} :
      D.enums[e]? = some ed → ed.variants[k]? = some Ts → HasTys D vs Ts →
      HasTy D (.enum e k i vs) (.enum e)

  /-- §6.1's `[ v1, …, vn ]` well typed at `[T; n]` exactly when it has `n`
  elements and each is well typed at `T` ((Array-Intro) §5.8, read on values;
  `3.5:3`'s one shared element type is `List.replicate n T`). The value
  carries `T` because `class([T; n])` is not a function of the elements
  present — `3.8:74` (`Syntax.lean`). -/
  | array {T n i vs} :
      HasTys D vs (List.replicate n T) → HasTy D (.array T i vs) (.array T n)

/-- Value typing for a value list, pointwise against the expected types: a
call's arguments against the callee's parameter types (§5.8's (Call),
`4.10:4`), a struct value's fields against its declared field list, and an
array value's elements against `n` copies of its element type. -/
inductive HasTys (D : Decls) : List Val → List Ty → Prop where
  | nil : HasTys D [] []
  | cons {v vs T Ts} : HasTy D v T → HasTys D vs Ts → HasTys D (v :: vs) (T :: Ts)
end

mutual
/-- Whether a contents tree has no `⊘` anywhere in it — the tree of a value
(helper). -/
def Contents.holeFree : Contents → Bool
  | .hole => false
  | .int _ _ _ | .float _ _ | .bool _ | .unit => true
  | .struct _ _ cs | .array _ _ cs => Contents.holeFreeList cs
  | .enum _ _ _ cs => Contents.holeFreeList cs

/-- The same over a field list (helper). -/
def Contents.holeFreeList : List Contents → Bool
  | [] => true
  | c :: cs => Contents.holeFree c && Contents.holeFreeList cs
end

mutual
/-- Cell contents typed against §2's types, with §6.1's `⊘` admitted at any
node. A moved-out position claims nothing, so it is well typed at every type;
every other node types as the corresponding value form does (§5.8's
(Struct-Intro), read on stored contents). -/
inductive ContentsTy (D : Decls) : Contents → Ty → Prop where
  | hole {T} : ContentsTy D .hole T
  | int {w s n} : InBounds w s n → ContentsTy D (.int w s n) (.int w s)
  /-- §6.1's `f_T` stored in a cell, with the same `𝔽_w` side condition
  `HasTy.float` carries. -/
  | float {w f} : f.Wf w → ContentsTy D (.float w f) (.float w)
  | bool {b} : ContentsTy D (.bool b) .bool
  | unit : ContentsTy D .unit .unit
  | struct {s sd i cs} :
      D.structs[s]? = some sd → ContentsTys D cs sd.fields →
      ContentsTy D (.struct s i cs) (.struct s)
  /-- §6.1's tagged value stored in a cell, typed the way `HasTy.enum` types
  the value. A payload position is never `⊘` in a reachable state (no path
  reaches one — `Dynamics.lean`), but nothing here needs that: `ContentsTy`
  admits `⊘` at every node and `holeFree` is what the rules that want a value
  ask for. -/
  | enum {e k ed Ts i cs} :
      D.enums[e]? = some ed → ed.variants[k]? = some Ts → ContentsTys D cs Ts →
      ContentsTy D (.enum e k i cs) (.enum e)

  /-- An array's stored contents: `n` positions, each well typed at the
  element type, with §6.1's `⊘` admitted at any of them. -/
  | array {T n i cs} :
      ContentsTys D cs (List.replicate n T) → ContentsTy D (.array T i cs) (.array T n)

/-- The same, pointwise against a declaration's field list, a variant's
payload components, or an array's `n` copies of its element type (§5.8's
(Struct-Intro)/(Array-Intro) and §5.5's (Enum-Intro), read on stored
contents). -/
inductive ContentsTys (D : Decls) : List Contents → List Ty → Prop where
  | nil : ContentsTys D [] []
  | cons {c cs T Ts} : ContentsTy D c T → ContentsTys D cs Ts → ContentsTys D (c :: cs) (T :: Ts)
end

mutual
/-- Per-node agreement between Σ's state for a path and the contents stored
there, at the path's declared type (§7's "Σ faithfully tracks the store's
initialization", section docstring): `owned` holds a value, `movedOut` holds
contents with no live linear sub-value — the §5.5 join's asymmetry, whose
residue the machine drops path-specifically (`3.8:60`) — and `fields` holds the
struct its type names, matched field by field. -/
inductive ContentsMatches (D : Decls) : Contents → OwnSt → Ty → Prop where
  /-- An `Owned` path holds a value: well-typed contents with no `⊘` in it. -/
  | owned {c T} : ContentsTy D c T → c.holeFree = true → ContentsMatches D c .owned T
  /-- A `MovedOut` path may still hold live contents — the §5.5 join's
  asymmetry (`3.8:60`) — but never a live linear sub-value (`3.8:50`). -/
  | moved {c T} :
      ContentsTy D c T → c.residualLinear D = false → ContentsMatches D c .movedOut T
  /-- A partially moved path holds the struct its type names, field by
  field. -/
  | fields {s sd i cs ts} :
      D.structs[s]? = some sd → ContentsMatchesList D cs ts sd.fields →
      ContentsMatches D (.struct s i cs) (.fields ts) (.struct s)
  /-- The array form of the same clause: a node with per-element records holds
  the array its type names, element by element. A constant-index **write**
  reaches it (`a[0] = …` records `fields [Owned]`), and so does an element
  move, whose `⊘` is what `3.8:73`'s per-path element drop reads. -/
  | elems {T n i cs ts} :
      ContentsMatchesList D cs ts (List.replicate n T) →
      ContentsMatches D (.array T i cs) (.fields ts) (.array T n)

/-- The same over a declaration's fields, slot by slot; a slot Σ has no record
for is `owned` (`OwnSt.fieldAt`) (helper). -/
inductive ContentsMatchesList (D : Decls) : List Contents → List OwnSt → List Ty → Prop where
  /-- No fields left to match. -/
  | nil {ts} : ContentsMatchesList D [] ts []
  /-- The first field matches its own slot's state; the rest match the tail of
  the record. -/
  | cons {c cs ts T Ts} :
      ContentsMatches D c (OwnSt.fieldAt ts 0) T → ContentsMatchesList D cs ts.tail Ts →
      ContentsMatchesList D (c :: cs) ts (T :: Ts)
end

/-- Per-cell agreement between the static entry and the dynamic cell: §7's
"Σ faithfully tracks the store's initialization", with the §5.5 join's
asymmetry built into `ContentsMatches`. A retired (`†`) cell matches no entry
at all, which is what keeps the unwind off one. -/
def CellMatches (D : Decls) (cell : Cell) (en : Entry) : Prop :=
  ∃ c, cell = .full c ∧ ContentsMatches D c en.st en.ty

/-- `Matches Γ ρ H`: each binding's location holds a cell agreeing with its
static entry; locations are live (in `H`) and pairwise distinct. This is the
§7 preservation invariant, over §6.1's environment `ρ` and store `H`. -/
inductive Matches (D : Decls) : Ctx → Env → Store → Prop where
  | nil {H} : Matches D [] [] H
  | cons {en : Entry} {Γ : Ctx} {ℓ : Nat} {ρ : Env} {H : Store} {c : Cell} :
      H[ℓ]? = some c → CellMatches D c en → ℓ ∉ ρ → Matches D Γ ρ H →
      Matches D (en :: Γ) (ℓ :: ρ) H

/-- `Untouched ρ H H'`: the store only grew, and every cell that was already
allocated and that `ρ` does not name has the contents it had. This is the
frame-locality property a call needs — a callee's cells are minted above the
caller's whole store (§6.9's (D-Call)), so the caller's bindings are outside
the callee's `ρ` and survive the call untouched. -/
def Untouched (ρ : Env) (H H' : Store) : Prop :=
  H.length ≤ H'.length ∧ ∀ ℓ, ℓ < H.length → ℓ ∉ ρ → H'[ℓ]? = H[ℓ]?

/-- The per-frame invariant (§6.1): the fused context agrees with the store
through the frame's environment, and the frame's scope record, read
newest-first, **is** that environment. The second clause is the RUE-1277
redundancy discharged — every live binding of the frame is registered for a
drop exactly once, which is what makes `run-all-scope-drops` (§6.9) safe at an
early `return`. -/
structure FrameMatches (D : Decls) (Γ : Ctx) (φ : Frame) (H : Store) : Prop where
  /-- `Matches` through the frame's environment `ρ`. -/
  store : Matches D Γ φ.env H
  /-- The scope record, newest-first, is the environment (`3.8:62`: every
  by-value binding is registered, and only those). In this fragment both are
  built from one list at every frame, so the equation holds definitionally;
  it becomes a real obligation when `Frame.scope` is §6.1's stack (§6.6,
  §6.10). -/
  record : φ.scope.reverse = φ.env

/-- The promise for an unwinding `break` (§6.10's (D-Break)), made about an
evaluation in frame `φ` from store `H`: the `break` fired in a frame that is
`φ` with some bindings `locs` opened on top of it — the ones the loop body
opened and had not closed, each minted above `H` — and at one of the
delivered states `B` of §5.3's `Ω`, the frame it fired in agrees with the
store. Its record is `φ`'s with `locs` appended, which is what the loop reads
to find the drops it owes, and nothing `φ` names outside the frame was
touched. It is closed under entering a binder (`BrokeOk.under_binders`), so a
`let` or a `match` arm passes it outward unchanged, and the loop that catches
it reads the frame it fired in straight off it (helper). -/
def BrokeOk (D : Decls) (B : List Ctx) (φ : Frame) (H H' : Store) (sc : List Nat) : Prop :=
  ∃ Γb ∈ B, ∃ locs : List Nat, sc = φ.scope ++ locs ∧
    FrameMatches D Γb { env := locs.reverse ++ φ.env, scope := sc } H' ∧
    (∀ ℓ ∈ locs, H.length ≤ ℓ) ∧ Untouched φ.env H H'

/-- The promise `soundness` makes about `eval`'s result, given §5.3's `Ω` —
its normal outgoing state `o` and its deliveries `B`: a value of the
expression's type with that state's invariant restored (preservation), or one
of `AbortOk`'s outcomes — never `.stuck` (progress). When `o` is `none`,
§5.7's `⊥`, a value is **impossible**: an expression the rules type as
divergent never completes normally. A `break` is one of the deliveries: the
state it fired at is one the rules recorded. Stating it as a predicate on the
result, rather than as a disjunction of existentials, is what lets the operand
combinators (`andThen`) be discharged once and reused at every form
(helper). -/
def EvalOk (D : Decls) (T R : Ty) (o : Option Ctx) (B : List Ctx) (φ : Frame) (H : Store) :
    EvalRes → Prop
  | .ok H' v _ =>
      match o with
      | some Γ' => HasTy D v T ∧ FrameMatches D Γ' φ H' ∧ Untouched φ.env H H'
      | none => False
  | .returned H' v _ => HasTy D v R ∧ Untouched φ.env H H'
  | .broke H' sc _ => BrokeOk D B φ H H' sc
  | .panic _ _ => True
  | .stuck _ => False
  | .outOfFuel => True

end RueCore
