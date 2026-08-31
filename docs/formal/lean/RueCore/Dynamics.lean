import RueCore.Statics

/-!
# RueCore.Dynamics — the executable machine (§6)

A total definitional interpreter over the §6.1 configuration shape, restricted
to the fragment: a store of single-cell binding allocations (`full v` / the
moved-out marker `⊘` = `moved` / the retired marker `†` = `dead`), an
environment mapping de Bruijn indices to locations, and a drop trace — the
fragment's image of the oracle's observable `Outcome` (drop trace + result).

Design commitments carried over from §6:

* **Memory violations are refusals, not silence.** Reading a `⊘`/`†` cell,
  implicitly dropping a linear value, or overwriting one, yields a named
  `Violation` — the fragment's stuck states. The §7 safety theorem
  (`Soundness.lean`) is exactly: well-typed programs never reach one.
* Scope exit *retires* the binding's allocation (`drop-retire`, §6.1), so a
  use after scope exit is `useAfterFree`, distinct from `useAfterMove`.
* `@drop` and overwrite-drop do **not** retire (§6.8/§6.11): the binding
  stays reinitializable.
* Arithmetic overflow and division by zero are *panics* (`↯` in §6.12), a
  defined outcome permitted by the safety theorem, not a violation.
* Traces record each drop (`drop ℓ v`) and each discarded temporary
  (`dropTemp v`) — the §6.7 temporary-death analog.

The interpreter is structurally recursive (the fragment has no loops or
calls), hence total: Lean's totality check is the fragment's termination
proof, and "progress" is absorbed into the shape of `EvalRes`.
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

/-- The dynamic image of `class(T)` on a value. -/
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

/-- Observable drop events — the fragment's slice of the oracle `Outcome`. -/
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
  | useAfterMove      -- reading a `⊘` cell (§7: no use-after-move)
  | useAfterFree      -- touching a `†` cell (§7: no use-after-drop)
  | linearLeak        -- scope exit on a live linear value (§7: consumed exactly once)
  | linearOverwrite   -- overwrite-drop of a live linear value (`3.8:77`)
  | linearDiscard     -- sequence-discard of a linear value (`3.8:64`)
  | unbound           -- dangling index (impossible for elaborated programs)
  | typeConfusion     -- operator on a wrong-shaped value
deriving DecidableEq, Repr

/-- Evaluation results: a value with the final store and trace, a defined
panic, or a violation ("stuck"). -/
inductive EvalRes where
  | ok (H : Store) (v : Val) (tr : List Event)
  | panic (k : PanicKind)
  | stuck (why : Violation)
deriving Repr

/-- Prefix a trace onto a result's trace. -/
def EvalRes.withTrace (tr : List Event) : EvalRes → EvalRes
  | .ok H v tr' => .ok H v (tr ++ tr')
  | r => r

/-- The interpreter. Rule correspondence, per case: `use` is
(D-Use-Copy)/(D-Use-Move) (§6.3); `add`/`div`/`lt` are §6.4 with its traps;
`drop` is §6.11's explicit `@drop`; `letIn` is (D-Let) + `endscope`'s
drop-retire (§6.7); `assign` is §6.8's overwrite-drop / reinitialization;
`seq` discards with a temporary drop (§6.7); `ite` is (D-If) (§6.2 search +
branch). -/
def eval (H : Store) (ρ : Env) : Expr → EvalRes
  | .intLit n => .ok H (.int n) []
  | .boolLit b => .ok H (.bool b) []
  | .unitLit => .ok H .unit []
  | .use i =>
      match ρ[i]? with
      | none => .stuck .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => .stuck .unbound
        | some .dead => .stuck .useAfterFree
        | some .moved => .stuck .useAfterMove
        | some (.full v) =>
            if v.mult = .copy then .ok H v []
            else .ok (H.set ℓ .moved) v []
  | .add e₁ e₂ =>
      match eval H ρ e₁ with
      | .ok H₁ (.int n₁) tr₁ =>
        (match eval H₁ ρ e₂ with
        | .ok H₂ (.int n₂) tr₂ =>
            if InBounds (n₁ + n₂) then .ok H₂ (.int (n₁ + n₂)) (tr₁ ++ tr₂)
            else .panic .overflow
        | .ok _ _ _ => .stuck .typeConfusion
        | r => r)
      | .ok _ _ _ => .stuck .typeConfusion
      | r => r
  | .div e₁ e₂ =>
      match eval H ρ e₁ with
      | .ok H₁ (.int n₁) tr₁ =>
        (match eval H₁ ρ e₂ with
        | .ok H₂ (.int n₂) tr₂ =>
            if n₂ = 0 then .panic .divZero
            else if InBounds (n₁.tdiv n₂) then .ok H₂ (.int (n₁.tdiv n₂)) (tr₁ ++ tr₂)
            else .panic .overflow
        | .ok _ _ _ => .stuck .typeConfusion
        | r => r)
      | .ok _ _ _ => .stuck .typeConfusion
      | r => r
  | .lt e₁ e₂ =>
      match eval H ρ e₁ with
      | .ok H₁ (.int n₁) tr₁ =>
        (match eval H₁ ρ e₂ with
        | .ok H₂ (.int n₂) tr₂ => .ok H₂ (.bool (decide (n₁ < n₂))) (tr₁ ++ tr₂)
        | .ok _ _ _ => .stuck .typeConfusion
        | r => r)
      | .ok _ _ _ => .stuck .typeConfusion
      | r => r
  | .mkres κ e =>
      match eval H ρ e with
      | .ok H' (.int n) tr => .ok H' (.res κ n) tr
      | .ok _ _ _ => .stuck .typeConfusion
      | r => r
  | .consume e =>
      match eval H ρ e with
      | .ok H' (.res _ n) tr => .ok H' (.int n) tr
      | .ok _ _ _ => .stuck .typeConfusion
      | r => r
  | .drop i =>
      match ρ[i]? with
      | none => .stuck .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => .stuck .unbound
        | some .dead => .stuck .useAfterFree
        | some .moved => .stuck .useAfterMove
        | some (.full v) =>
            if v.mult = .copy then .ok H .unit []
            else .ok (H.set ℓ .moved) .unit [.drop ℓ v]
  | .letIn _m e₁ e₂ =>
      match eval H ρ e₁ with
      | .ok H₁ v₁ tr₁ =>
          -- (D-Let): mint a fresh single-cell binding allocation.
          (match eval (H₁ ++ [.full v₁]) (H₁.length :: ρ) e₂ with
          | .ok H₂ v₂ tr₂ =>
              -- endscope: §5.6's obligations, executed. Retire the cell (§6.1).
              (match H₂[H₁.length]? with
              | some (.full v') =>
                  match v'.mult with
                  | .linear => .stuck .linearLeak
                  | .affine => .ok (H₂.set H₁.length .dead) v₂
                      (tr₁ ++ tr₂ ++ [.drop H₁.length v'])
                  | .copy => .ok (H₂.set H₁.length .dead) v₂ (tr₁ ++ tr₂)
              | some .moved => .ok (H₂.set H₁.length .dead) v₂ (tr₁ ++ tr₂)
              | some .dead => .stuck .useAfterFree
              | none => .stuck .unbound)
          | r => r.withTrace tr₁)
      | r => r
  | .assign i e =>
      match eval H ρ e with
      | .ok H₁ v tr =>
          (match ρ[i]? with
          | none => .stuck .unbound
          | some ℓ =>
            match H₁[ℓ]? with
            | none => .stuck .unbound
            | some .dead => .stuck .useAfterFree
            | some .moved => .ok (H₁.set ℓ (.full v)) .unit tr    -- reinit (3.8:55)
            | some (.full vOld) =>
                match vOld.mult with
                | .linear => .stuck .linearOverwrite               -- 3.8:77
                | .affine => .ok (H₁.set ℓ (.full v)) .unit (tr ++ [.drop ℓ vOld])
                | .copy => .ok (H₁.set ℓ (.full v)) .unit tr)
      | r => r
  | .seq e₁ e₂ =>
      match eval H ρ e₁ with
      | .ok H₁ v₁ tr₁ =>
          (match v₁.mult with
          | .linear => .stuck .linearDiscard                       -- 3.8:64
          | .affine => (eval H₁ ρ e₂).withTrace (tr₁ ++ [.dropTemp v₁])
          | .copy => (eval H₁ ρ e₂).withTrace tr₁)
      | r => r
  | .ite c e₁ e₂ =>
      match eval H ρ c with
      | .ok H₀ (.bool b) tr₀ =>
          (if b then (eval H₀ ρ e₁).withTrace tr₀
           else (eval H₀ ρ e₂).withTrace tr₀)
      | .ok _ _ _ => .stuck .typeConfusion
      | r => r

end RueCore
