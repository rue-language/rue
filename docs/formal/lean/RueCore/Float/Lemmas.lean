module

public import RueCore.Float

@[expose] public section

/-!
# RueCore.Float.Lemmas — `Float.exactOps` satisfies the laws (layer L2)

`FloatModel` (`Float.lean`) is a `FloatOps` together with the laws §7's
"totality of the float operations" lemma names, and 19 of the spine's
statements quantify over one. Were the laws jointly unsatisfiable, those
statements would hold vacuously (RUE-2469). This module proves every law of
the executable instance `Float.exactOps` and packages it as
`Float.exactModel : FloatModel`, so the laws have a model, and it is the
model the corpus and the printer run on.

* **Closure** (`arith_wf`, `sqrt_wf`, `ofLit_wf`, `ofInt_wf`, `narrow_wf`):
  every rounded result is `roundRat` of an exact rational with a non-zero
  denominator, or a special, or `@sqrt`'s own rounding. `roundRat_wf` is the
  core: `log2Floor` bounds the rational below `2^(e₀+1)`, so the significand
  rounded at exponent `e ≥ e₀ - p + 1` is at most `2^p`, and `roundRat`'s
  renormalization, underflow and overflow tests do the rest. `sqrt_core` is
  `@sqrt`'s: the root of a magnitude below `2^eTop` is below `2^(eTop/2)`,
  so it never overflows, and its exponent stays above the subnormal floor.
* **The behavioural laws** (`arith_nan`, `narrow_nan`, `div_by_zero`,
  `zero_div_zero`, `ofLit_zero`, `ofLit_one`) are case analysis on the
  definitions; `ofLit_one` evaluates `roundRat` at each width in the kernel
  (`decide +kernel`, no `native_decide`: the exponents reach `2^1076`, past the
  elaborator's evaluation threshold).

Three facts of Lean's core library that the proofs need reach
`Classical.choice` there (`Nat.lt_of_mul_lt_mul_right`,
`Nat.pow_lt_pow_right`, `Nat.sqrt_le`), so they are reproved constructively
here, and every theorem of this module rests on `propext` and `Quot.sound`
only.
-/

namespace RueCore

/-- Cancel a common right factor from a strict inequality, without the core library's `Classical.choice` (helper). -/
theorem lt_of_mul_lt_mul_right' {a b c : Nat} (h : a * c < b * c) : a < b :=
  Nat.lt_of_not_le fun hle => Nat.not_lt_of_le (Nat.mul_le_mul_right c hle) h

/-- `2^a < 2^b` when `a < b`, constructively (helper). -/
theorem two_pow_lt_two_pow {a b : Nat} (h : a < b) : 2 ^ a < 2 ^ b := by
  have h1 : 2 ^ (a + 1) ≤ 2 ^ b := Nat.pow_le_pow_right (by decide) h
  have h2 : 2 ^ (a + 1) = 2 ^ a * 2 := Nat.pow_succ ..
  have h3 := Nat.two_pow_pos a
  omega

/-- `a ≤ b` when `2^a ≤ 2^b`, constructively (helper). -/
theorem le_of_two_pow_le {a b : Nat} (h : 2 ^ a ≤ 2 ^ b) : a ≤ b :=
  Nat.le_of_not_lt fun hlt => Nat.not_lt_of_le h (two_pow_lt_two_pow hlt)

/-- Newton's iteration for `Nat.sqrt` never overshoots: its result squared is at most `n` (helper). -/
theorem sqrt_iter_sq_le (n : Nat) : ∀ guess, Nat.sqrt.iter n guess * Nat.sqrt.iter n guess ≤ n := by
  intro guess
  induction guess using Nat.strongRecOn with
  | ind g ih =>
    rw [Nat.sqrt.iter.eq_1]
    split
    · next h => exact ih _ h
    · next h =>
        apply Nat.mul_le_of_le_div
        omega

/-- `Nat.sqrt n` squared is at most `n`, proved here because the core
library's `Nat.sqrt_le` reaches `Classical.choice` (helper). -/
theorem sqrt_sq_le (n : Nat) : Nat.sqrt n * Nat.sqrt n ≤ n := by
  unfold Nat.sqrt
  split
  · next h => exact Nat.le_trans (Nat.mul_le_mul_right _ h) (by rw [Nat.one_mul]; exact Nat.le_refl _)
  · exact sqrt_iter_sq_le _ _

/-- `n` is below `2` to the power of its bit length (helper). -/
theorem bitLen_lt (n : Nat) : n < 2 ^ bitLen n := by
  unfold bitLen
  split
  · next h => subst h; decide
  · exact Nat.lt_log2_self

/-- A non-zero `n` is at least `2` to the power of its bit length less one (helper). -/
theorem pow_bitLen_pred_le {n : Nat} (h : n ≠ 0) : 2 ^ (bitLen n - 1) ≤ n := by
  unfold bitLen
  rw [if_neg h, Nat.add_sub_cancel]
  exact Nat.log2_self_le h

/-- Move powers of two across a strict inequality `a·2^X < b·2^Y`: it holds with `X', Y'` whenever `X' - Y' ≤ X - Y` (helper). -/
theorem pow_scale {a b X Y X' Y' : Nat} (h : a * 2 ^ X < b * 2 ^ Y) (hle : X' + Y ≤ X + Y') :
    a * 2 ^ X' < b * 2 ^ Y' := by
  have h1 : a * 2 ^ X * 2 ^ X' < b * 2 ^ Y * 2 ^ X' :=
    Nat.mul_lt_mul_of_pos_right h (Nat.two_pow_pos _)
  have h2 : b * 2 ^ Y * 2 ^ X' ≤ b * 2 ^ Y' * 2 ^ X := by
    rw [Nat.mul_assoc, Nat.mul_assoc, ← Nat.pow_add, ← Nat.pow_add]
    exact Nat.mul_le_mul_left _ (Nat.pow_le_pow_right (by decide) (by omega))
  have h3 : a * 2 ^ X' * 2 ^ X < b * 2 ^ Y' * 2 ^ X := by
    calc a * 2 ^ X' * 2 ^ X = a * 2 ^ X * 2 ^ X' := by
          rw [Nat.mul_assoc, Nat.mul_assoc, Nat.mul_comm (2 ^ X')]
      _ < b * 2 ^ Y * 2 ^ X' := h1
      _ ≤ b * 2 ^ Y' * 2 ^ X := h2
  exact lt_of_mul_lt_mul_right' h3

/-- `log2Floor` is not too small: `num/den < 2^(log2Floor num den + 1)`, written over naturals (helper). -/
theorem log2Floor_lt {num den : Nat} (hd : den ≠ 0) :
    num * 2 ^ (-(log2Floor num den + 1)).toNat < den * 2 ^ (log2Floor num den + 1).toNat := by
  unfold log2Floor
  dsimp only
  split
  · -- `num < 2^bn` and `2^(bd-1) ≤ den`
    have hlt := bitLen_lt num
    have hle := pow_bitLen_pred_le hd
    have hbd : bitLen den ≠ 0 := by
      intro h0; unfold bitLen at h0; rw [if_neg hd] at h0; omega
    have key := pow_scale (a := num) (b := 1) (X := 0) (Y := bitLen num)
      (X' := (-((bitLen num : Int) - (bitLen den : Int) + 1)).toNat)
      (Y' := bitLen den - 1 + ((bitLen num : Int) - (bitLen den : Int) + 1).toNat)
      (by simpa using hlt) (by omega)
    rw [Nat.one_mul, Nat.pow_add] at key
    exact Nat.lt_of_lt_of_le key (Nat.mul_le_mul_right _ hle)
  · next hlt =>
      have e1 : (max 0 (-((bitLen num : Int) - (bitLen den : Int)))).toNat =
          (-((bitLen num : Int) - (bitLen den : Int) - 1 + 1)).toNat := by omega
      have e2 : (max 0 ((bitLen num : Int) - (bitLen den : Int))).toNat =
          ((bitLen num : Int) - (bitLen den : Int) - 1 + 1).toNat := by omega
      rw [e1, e2] at hlt
      exact Nat.lt_of_not_le hlt


/-- Rounding `a/b` to nearest stays at or below `K` when `a < b·K` (helper). -/
theorem roundDivHalfEven_le {a b K : Nat} (hb : 0 < b) (h : a < b * K) :
    roundDivHalfEven a b ≤ K := by
  have hq : a / b < K := (Nat.div_lt_iff_lt_mul hb).mpr (by rw [Nat.mul_comm]; exact h)
  unfold roundDivHalfEven
  dsimp only
  split
  · omega
  · split
    · omega
    · split <;> omega

/-- Every width has a positive precision (helper). -/
theorem pow_prec_pos (w : FloatWidth) : 0 < w.prec := by cases w <;> decide

/-- The last step of `roundRat` lands in `𝔽_w` whenever the rounded significand is at most `2^p` and the exponent is at or above the subnormal floor: renormalizing, the underflow to zero and the overflow test cover the rest (helper). -/
theorem roundTail_wf {w : FloatWidth} {neg : Bool} {m : Nat} {e : Int}
    (hm : m ≤ 2 ^ w.prec) (he : w.eMin ≤ e) :
    (let (m, e) := if m = 2 ^ w.prec then (2 ^ (w.prec - 1), e + 1) else (m, e)
     if m = 0 then FloatDatum.num neg 0 0
     else if w.eTop < e ∨ 2 ^ (w.eTop - e).toNat ≤ m then .inf neg
     else canonNum neg m e).Wf w := by
  have hlast : ∀ m' e', m' < 2 ^ w.prec → w.eMin ≤ e' →
      (if m' = 0 then FloatDatum.num neg 0 0
       else if w.eTop < e' ∨ 2 ^ (w.eTop - e').toNat ≤ m' then .inf neg
       else canonNum neg m' e').Wf w := by
    intro m' e' hm' he'
    split
    · exact Or.inl ⟨rfl, rfl⟩
    · split
      · trivial
      · next hc =>
          exact canonNum_wf hm' he' (by omega) (Nat.lt_of_not_le (fun h => hc (Or.inr h)))
  by_cases heq : m = 2 ^ w.prec
  · rw [if_pos heq]
    refine hlast (2 ^ (w.prec - 1)) (e + 1) ?_ (by omega)
    exact two_pow_lt_two_pow (by have := pow_prec_pos w; omega)
  · rw [if_neg heq]
    exact hlast m e (by omega) he

/-- **`rnd_w` lands in `𝔽_w`** (§2, §7's "`rnd_w` is total into `𝔽_w`"):
`roundRat` of any rational with a non-zero denominator is a datum of the
width. -/
theorem roundRat_wf (w : FloatWidth) (neg : Bool) (num : Nat) {den : Nat} (hd : den ≠ 0) :
    (roundRat w neg num den).Wf w := by
  unfold roundRat
  split
  · exact Or.inl ⟨rfl, rfl⟩
  · next hn =>
      dsimp only
      apply roundTail_wf _ (Int.le_max_left _ _)
      apply Nat.le_of_lt_succ
      apply Nat.lt_succ_of_le
      apply roundDivHalfEven_le (Nat.mul_pos (Nat.pos_of_ne_zero hd) (Nat.two_pow_pos _))
      have key := log2Floor_lt (num := num) hd
      rw [Nat.mul_assoc]
      rw [← Nat.pow_add]
      refine pow_scale key ?_
      omega

/-- A power of two is not zero (helper). -/
theorem two_pow_ne_zero {n : Nat} : 2 ^ n ≠ 0 := Nat.pos_iff_ne_zero.mp (Nat.two_pow_pos n)

/-- A number below `2^E` has at most `E` bits (helper). -/
theorem bitLen_le_of_lt {n E : Nat} (h : n < 2 ^ E) : bitLen n ≤ E := by
  unfold bitLen
  split
  · exact Nat.zero_le _
  · next hn => exact Nat.succ_le_of_lt ((Nat.log2_lt hn).mpr h)

/-- `r < 2^E` when `r·r < 2^E·2^E` (helper). -/
theorem lt_pow_of_sq_lt {r E : Nat} (h : r * r < 2 ^ E * 2 ^ E) : r < 2 ^ E :=
  Nat.lt_of_not_le fun hle => Nat.lt_irrefl _ (Nat.lt_of_lt_of_le h (Nat.mul_le_mul hle hle))

/-- The last step of `Float.sqrtD` lands in `𝔽_w`: whatever the rounding
decision, the root `r` of `sig · 2^(exp - 2t)` scaled back by `2^(t + k)` is
within the width's range, because the square root of a finite magnitude below
`2^eTop` is below `2^(eTop/2)` (helper). -/
theorem sqrt_core {w : FloatWidth} {sig : Nat} {exp : Int}
    (htop : sig < 2 ^ (w.eTop - exp).toNat) (hlo : w.eMin ≤ exp) (hhi : exp ≤ w.eTop)
    {t : Int} (ht : t = Int.fdiv exp 2 - (w.prec : Int) - 2)
    {r : Nat} (hr : r * r ≤ sig * 2 ^ (exp - 2 * t).toNat)
    {m : Nat} (hm : m ≤ r / 2 ^ (bitLen r - w.prec) + 1) :
    (if m = 2 ^ w.prec then canonNum false (2 ^ (w.prec - 1)) (t + ((bitLen r - w.prec : Nat) : Int) + 1)
     else canonNum false m (t + ((bitLen r - w.prec : Nat) : Int))).Wf w := by
  have hfd : Int.fdiv exp 2 = exp / 2 := Int.fdiv_eq_ediv_of_nonneg _ (by decide)
  rw [hfd] at ht
  have hEven : w.eTop / 2 * 2 = w.eTop := by cases w <;> decide
  have hp := pow_prec_pos w
  have hpE : (w.prec : Int) + 2 ≤ w.eTop / 2 := by cases w <;> decide
  have hlo' : w.eMin + 2 * (w.prec : Int) + 6 ≤ w.eMin / 2 := by cases w <;> decide
  -- `E = eTop/2 - t`, the root's bit budget
  let E : Nat := (w.eTop / 2 - t).toNat
  have hE : (E : Int) = w.eTop / 2 - t := by omega
  have ha : sig * 2 ^ (exp - 2 * t).toNat < 2 ^ E * 2 ^ E := by
    rw [← Nat.pow_add]
    have h1 := Nat.mul_lt_mul_of_pos_right htop (Nat.two_pow_pos (exp - 2 * t).toNat)
    rw [← Nat.pow_add] at h1
    have hx : (w.eTop - exp).toNat + (exp - 2 * t).toNat = E + E := by omega
    rwa [hx] at h1
  have hrE : r < 2 ^ E := lt_pow_of_sq_lt (Nat.lt_of_le_of_lt hr ha)
  have hbl : bitLen r ≤ E := bitLen_le_of_lt hrE
  let k := bitLen r - w.prec
  have hkE : k ≤ E := by omega
  have hrk : r < 2 ^ w.prec * 2 ^ k := by
    rw [← Nat.pow_add]
    exact Nat.lt_of_lt_of_le (bitLen_lt r) (Nat.pow_le_pow_right (by decide) (by omega))
  have hm0 : r / 2 ^ k < 2 ^ w.prec := (Nat.div_lt_iff_lt_mul (Nat.two_pow_pos _)).mpr hrk
  have hm0E : r / 2 ^ k < 2 ^ (E - k) := by
    apply (Nat.div_lt_iff_lt_mul (Nat.two_pow_pos _)).mpr
    rw [← Nat.pow_add, Nat.sub_add_cancel hkE]; exact hrE
  have hm' : m ≤ r / 2 ^ k + 1 := hm
  have hmE : m ≤ 2 ^ (E - k) := by omega
  have htlo : w.eMin ≤ t := by omega
  split
  · next heq =>
      have hpk : w.prec ≤ E - k :=
        le_of_two_pow_le (heq ▸ hmE)
      apply canonNum_wf
      · exact two_pow_lt_two_pow (by omega)
      · omega
      · omega
      · exact two_pow_lt_two_pow (by omega)
  · next hne =>
      apply canonNum_wf
      · omega
      · omega
      · omega
      · exact Nat.lt_of_le_of_lt hmE (two_pow_lt_two_pow (by omega))

namespace Float

/-- `ratAdd`'s denominator is the product of the two (helper). -/
theorem ratAdd_den (n₁ : Bool) (a₁ b₁ : Nat) (n₂ : Bool) (a₂ b₂ : Nat) :
    (ratAdd n₁ a₁ b₁ n₂ a₂ b₂).2.2 = b₁ * b₂ := by
  unfold ratAdd; dsimp only; split
  · rfl
  · split <;> rfl

/-- `+` lands in `𝔽_w` (helper). -/
theorem addD_wf (σ : Bool) (w : FloatWidth) (a b : FloatDatum) : (addD σ w a b).Wf w := by
  cases a <;> cases b <;> simp only [addD]
  all_goals (repeat' split) <;>
    first
      | trivial
      | exact Or.inl ⟨rfl, rfl⟩
      | exact roundRat_wf _ _ _ (by rw [ratAdd_den]; exact Nat.mul_ne_zero two_pow_ne_zero two_pow_ne_zero)

/-- `*` lands in `𝔽_w` (helper). -/
theorem mulD_wf (σ : Bool) (w : FloatWidth) (a b : FloatDatum) : (mulD σ w a b).Wf w := by
  cases a <;> cases b <;> simp only [mulD]
  all_goals (repeat' split) <;>
    first
      | trivial
      | exact Or.inl ⟨rfl, rfl⟩
      | exact roundRat_wf _ _ _ (Nat.mul_ne_zero two_pow_ne_zero two_pow_ne_zero)

/-- `/` lands in `𝔽_w` (helper). -/
theorem divD_wf (σ : Bool) (w : FloatWidth) (a b : FloatDatum) : (divD σ w a b).Wf w := by
  cases a <;> cases b <;> simp only [divD]
  all_goals (repeat' split) <;>
    first
      | trivial
      | exact Or.inl ⟨rfl, rfl⟩
      | exact roundRat_wf _ _ _ (Nat.mul_ne_zero two_pow_ne_zero
          (Nat.mul_ne_zero (by assumption) two_pow_ne_zero))
  done

/-- `-` lands in `𝔽_w` (helper). -/
theorem subD_wf (σ : Bool) (w : FloatWidth) (a b : FloatDatum) : (subD σ w a b).Wf w := by
  cases a <;> cases b <;> simp only [subD] <;> first | trivial | exact addD_wf _ _ _ _

/-- **Closure of `⊕_w`** (§7) for `exactOps`: `FloatModel.arith_wf`, with no hypothesis on the operands. -/
theorem arith_wf (σ : Bool) (w : FloatWidth) (op : FloatArith) (a b : FloatDatum) :
    (arith σ w op a b).Wf w := by
  cases op
  · exact addD_wf _ _ _ _
  · exact subD_wf _ _ _ _
  · exact mulD_wf _ _ _ _
  · exact divD_wf _ _ _ _

/-- **Closure of the narrowing cast** (`3.12:19`) for `exactOps`: `FloatModel.narrow_wf`, on any datum. -/
theorem narrow_wf (f : FloatDatum) : (narrow f).Wf .w32 := by
  cases f <;> simp only [narrow]
  all_goals (repeat' split) <;>
    first
      | trivial
      | exact Or.inl ⟨rfl, rfl⟩
      | exact roundRat_wf _ _ _ two_pow_ne_zero

/-- **Closure of `rnd_w` on an integer** (`3.12:16`) for `exactOps`: `FloatModel.ofInt_wf`. -/
theorem ofInt_wf (w : FloatWidth) (n : Int) : (ofInt w n).Wf w :=
  roundRat_wf _ _ _ Nat.one_ne_zero

/-- **Closure of `rnd_w` on a literal** (`3.12:9`) for `exactOps`: `FloatModel.ofLit_wf`. -/
theorem ofLit_wf (w : FloatWidth) (m : Nat) (ne : Bool) (e : Nat) : (ofLit w m ne e).Wf w := by
  unfold ofLit FloatLit.exact
  cases ne
  · exact roundRat_wf _ _ _ Nat.one_ne_zero
  · exact roundRat_wf _ _ _ (Nat.pos_iff_ne_zero.mp (Nat.pow_pos (by decide)))

/-- **A NaN operand yields a NaN** for `exactOps` (`FloatModel.arith_nan`): the operand is propagated. -/
theorem arith_nan (σ : Bool) (w : FloatWidth) (op : FloatArith) (a b : FloatDatum)
    (h : a.isNaN = true ∨ b.isNaN = true) : (arith σ w op a b).isNaN = true := by
  cases op <;> cases a <;> cases b <;>
    simp_all [arith, addD, subD, mulD, divD, FloatDatum.isNaN]

/-- **A cast of a NaN is a NaN** for `exactOps` (`FloatModel.narrow_nan`). -/
theorem narrow_nan (f : FloatDatum) (h : f.isNaN = true) : (narrow f).isNaN = true := by
  cases f <;> simp_all [narrow, FloatDatum.isNaN]

/-- **A finite non-zero over a zero is the infinity of the xor sign** (`3.12:22`) for `exactOps` (`FloatModel.div_by_zero`). -/
theorem div_by_zero (σ : Bool) (w : FloatWidth) (n : Bool) (s : Nat) (e : Int) (hs : s ≠ 0)
    (n₂ : Bool) : arith σ w .div (.num n s e) (.num n₂ 0 0) = .inf (xor n n₂) := by
  simp [arith, divD, hs]

/-- **`0/0` is `NaN(σ_NaN)`** (`3.12:22`) for `exactOps` (`FloatModel.zero_div_zero`). -/
theorem zero_div_zero (σ : Bool) (w : FloatWidth) (n₁ n₂ : Bool) :
    arith σ w .div (.num n₁ 0 0) (.num n₂ 0 0) = .nan σ := by
  simp [arith, divD]

/-- **The decimal zero is `+0`** (`3.12:9`) for `exactOps` (`FloatModel.ofLit_zero`). -/
theorem ofLit_zero (w : FloatWidth) (ne : Bool) (e : Nat) : ofLit w 0 ne e = .num false 0 0 := by
  cases ne <;> simp [ofLit, FloatLit.exact, roundRat]

/-- **The decimal one is `1 · 2^0`** (`3.12:9`) for `exactOps` (`FloatModel.ofLit_one`), evaluated in the kernel at each width. -/
theorem ofLit_one (w : FloatWidth) : ofLit w 1 false 0 = .num false 1 0 := by
  cases w <;> decide +kernel

/-- Rounding up adds at most one (helper). -/
theorem ite_succ_le (c : Prop) [Decidable c] (x : Nat) : (if c then x + 1 else x) ≤ x + 1 := by
  split
  · exact Nat.le_refl _
  · exact Nat.le_succ _

/-- **Closure of `@sqrt`** (`3.12:35`, §7's "each `⊙_w` is total on `𝔽_w`") for `exactOps`: `FloatModel.sqrt_wf`. -/
theorem sqrt_wf (σ : Bool) (w : FloatWidth) (f : FloatDatum) (hf : f.Wf w) :
    (sqrtD σ w f).Wf w := by
  cases f with
  | nan _ => trivial
  | inf b => cases b <;> trivial
  | num neg sig exp =>
      unfold sqrtD
      dsimp only
      split
      · exact Or.inl ⟨rfl, rfl⟩
      · next hs =>
          split
          · trivial
          · rcases hf with ⟨h0, _⟩ | ⟨_, _, hlo, hhi, htop⟩
            · exact absurd h0 hs
            · exact sqrt_core htop hlo hhi rfl (sqrt_sq_le _) (ite_succ_le _ _)

/-- **The laws have a model: `Float.exactOps`.** Every field of `FloatModel` proved of the executable instance, so the 19 spine statements that quantify over `M : FloatModel` are not vacuous (RUE-2469), and each applies to the model the corpus runs on. What the laws leave open, `exactOps` still decides by choice — which NaN a propagating operation returns, and `σ_NaN` — and those choices are checked against the compiler by the corpus, not proved. -/
def exactModel : FloatModel where
  toFloatOps := exactOps
  arith_wf w op a b _ _ := arith_wf false w op a b
  sqrt_wf w f hf := sqrt_wf false w f hf
  ofLit_wf := ofLit_wf
  ofInt_wf := ofInt_wf
  narrow_wf f _ := narrow_wf f
  arith_nan w op a b h := arith_nan false w op a b h
  narrow_nan := narrow_nan
  div_by_zero w a n s e _ ha hs n₂ := by subst ha; exact div_by_zero false w n s e hs n₂
  zero_div_zero w n₁ n₂ := zero_div_zero false w n₁ n₂
  ofLit_zero := ofLit_zero
  ofLit_one := ofLit_one

end Float

end RueCore
