/-!
# RueCore.Float — `𝔽_w`, the rendering, and the `FloatModel` interface

§2's representation decision, mechanized: a value of `float(w)` is an
**abstract IEEE 754 binary-`w` datum, not a bit pattern**. `FloatDatum` is
exactly §2's set

```
  𝔽_w = { the finite values of binary-w, with -0 and +0 distinct }
        ∪ { -inf, +inf } ∪ { NaN(-), NaN(+) }
```

with a NaN carrying a **sign and no payload** (§2; the residual distance from
`3.12:32`'s "same bit pattern" wording is §9 item 5, and nothing a core program
can write observes a payload). A finite datum is `num neg sig exp`, the number
`(-1)^neg · sig · 2^exp`, kept **canonical** — `sig = 0` forces `exp = 0`, so
`±0` are two data and no more, and a non-zero `sig` is odd, so each non-zero
number has exactly one representation. Canonicity is what makes structural
equality *datum* equality, which `(D-Total-Cmp)`'s `k = 0` case needs
(§6.4: "`k = 0` holds exactly when the two operands are the same datum").

`FloatDatum` is deliberately **not** Lean's `Float`. Lean 4.33.1 defines
`Float` over the `opaque` constant `floatSpec`, so merely *mentioning* a
`Float` in a theorem's statement makes the kernel appeal to `Classical.choice`
— measured, not assumed: `theorem t : ∀ f : Float, g f = g f := fun _ => rfl`
already reports `[propext, Classical.choice, Quot.sound]`. Carrying a `Float`
inside `Val` would therefore have put `Classical.choice` on *every* theorem of
`Soundness.lean`, breaking this package's constructive policy (`TRUST.md`). A
datum built from `Nat`/`Int` costs nothing.

## Which operations are defined here, and which the model supplies

§6.4 splits the float operations in two, and this module splits with it.

* **Exact** — no rounding is involved, so the operation is a *function* here
  and its properties are theorems rather than assumptions: `neg` (a sign flip,
  `3.12:24`), the ordering compares (`(D-Float-Ord)`, `3.12:27`, `3.12:28`),
  `@total_cmp` (`(D-Total-Cmp)`; the order is §6.4's chain on the datum),
  `@float_to_int` (truncation toward zero is exact integer arithmetic, so
  `(D-Float-To-Int)`/`(D-Float-To-Int-Trap)`'s partition is proved —
  `floatToInt_partition` — rather than assumed), `@floor`/`@ceil`/`@trunc`/
  `@round` (`3.12:36`: "each result is exact"), and the widening half of
  `@float_cast` (`3.12:19`: widening is value-preserving, so it is the
  identity on the datum).
* **Rounded** — the result is `rnd_w` of an exact value that need not be in
  `𝔽_w`, which is §2's second model parameter: the four arithmetic operators,
  `@sqrt`, `@int_to_float`, a literal's own conversion (`3.12:9`), and the
  narrowing half of `@float_cast`. Those are the fields of `FloatOps`, and
  `FloatModel` is a `FloatOps` together with the laws §7's "totality of the
  float operations" lemma names. The machine (`Dynamics.lean`) takes a
  `FloatOps`; every theorem quantifies over a `FloatModel`, so it holds for
  every model satisfying the laws.

`Float.exactOps` is the executable instance the corpus and the printer run on.
It is **constructive**: `roundRat` rounds an exact rational to `𝔽_w` by
integer arithmetic, so `+ - * /` (exact rationals, then `rnd_w`) and `@sqrt`
(an integer square root, then `rnd_w`) need no host float and no axiom. What
the package does *not* prove is that `exactOps` satisfies the laws — that is
the residual assumption `TRUST.md` records, and it is discharged the way §7
says the totality lemma is: against IEEE 754, and here also against the
compiler, case by case, by the corpus.

## The rendering

`FloatDatum.render` is `3.12:40`–`3.12:42`: the shortest decimal digit string
that round-trips at the value's own width, laid out positionally when the
leading digit's decimal exponent is in `-5..=15` (f64) or `-6..=12` (f32) and
in `d.ddde±XX` otherwise, with `NaN` / `inf` / `-inf` / `-0.0` spelled out.
The compiler realizes the same paragraphs through the vendored `zmij`
formatter (`crates/rue-runtime/src/string.rs`), and the two were compared by
hand on the probe table in this slice's report before this code was written.
A NaN renders `NaN` whatever its sign, which is why `σ_NaN` — a *target*
parameter (§2) — never reaches a corpus expectation through `@dbg`.

The shortest-round-trip search is exact: the rounding interval of a datum is
computed in integers (`ulp`, the half-ulp below being a quarter-ulp at a
power of two, and the endpoints included exactly when the IEEE significand is
even — `rnd_w` is ties-to-even, §2), and a candidate is accepted only when it
lies in that interval. No floating-point arithmetic is used to decide it.
-/

namespace RueCore

/-! ## Widths -/

/-- The float widths §2's `float(w)` ranges over: `w ∈ {32, 64}`, the surface
`f32` and `f64` (`3.12:1`). -/
inductive FloatWidth where
  | w32
  | w64
deriving DecidableEq, Repr

/-- `w` as a number of bits (§2) (helper). -/
def FloatWidth.bits : FloatWidth → Nat
  | .w32 => 32
  | .w64 => 64

/-- The significand precision `p` of binary-`w`: 24 bits for `f32`, 53 for
`f64` (IEEE 754) (helper). -/
def FloatWidth.prec : FloatWidth → Nat
  | .w32 => 24
  | .w64 => 53

/-- The least exponent a value of `𝔽_w` can be written `m · 2^e` with: the
subnormal floor, `-149` for `f32` and `-1074` for `f64` (helper). -/
def FloatWidth.eMin : FloatWidth → Int
  | .w32 => -149
  | .w64 => -1074

/-- One past the binary exponent of the largest finite magnitude: `max_{𝔽_w}`
is `(2^p - 1) · 2^(eTop - p)`, so a canonical `sig · 2^exp` is finite exactly
when `exp + bitLen sig ≤ eTop` (helper). -/
def FloatWidth.eTop : FloatWidth → Int
  | .w32 => 128
  | .w64 => 1024

/-- The most significant decimal digits a shortest round-trip rendering can
need at this width (`MAX_DIGITS10`: 9 for `f32`, 17 for `f64`) (helper). -/
def FloatWidth.maxDigits : FloatWidth → Nat
  | .w32 => 9
  | .w64 => 17

/-- The low end of `3.12:41`'s positional window, on the decimal exponent of
the leading digit: `-6` for `f32`, `-5` for `f64` (helper). -/
def FloatWidth.fixedLo : FloatWidth → Int
  | .w32 => -6
  | .w64 => -5

/-- The high end of `3.12:41`'s positional window: `12` for `f32`, `15` for
`f64` (helper). -/
def FloatWidth.fixedHi : FloatWidth → Int
  | .w32 => 12
  | .w64 => 15

/-! ## The datum set `𝔽_w` -/

/-- §2's `𝔽_w`. `num neg sig exp` is the number `(-1)^neg · sig · 2^exp`;
`inf neg` are the two infinities; `nan neg` is `NaN(σ)`, which carries a sign
and no payload (§2, §9 item 5). Canonical data — what every operation here
produces and `Wf` requires — have `sig = 0 → exp = 0` (so `±0` are exactly two
data) and `sig ≠ 0 → sig` odd (so each non-zero number has one
representation). -/
inductive FloatDatum where
  | nan (neg : Bool)
  | inf (neg : Bool)
  | num (neg : Bool) (sig : Nat) (exp : Int)
deriving DecidableEq, Repr

/-- The number of bits in `n`'s binary representation, `0` for `0`
(helper). -/
def bitLen (n : Nat) : Nat := if n = 0 then 0 else Nat.log2 n + 1

/-- The canonicalization loop, structural on a budget: a significand below
`2^k` loses at most `k` factors of two (helper). -/
def canonAux : Nat → Bool → Nat → Int → FloatDatum
  | 0, neg, sig, exp => .num neg sig exp
  | n + 1, neg, sig, exp =>
      if sig % 2 = 0 then canonAux n neg (sig / 2) (exp + 1) else .num neg sig exp

/-- Strip the factors of two out of `sig`, moving them into `exp`, and send a
zero significand to the one `±0` of its sign: the canonical form of
`(-1)^neg · sig · 2^exp`. The budget is `sig` itself, which is more than
enough (`Nat.lt_two_pow_self`) and costs nothing, since the loop stops at the
first odd significand (helper). -/
def canonNum (neg : Bool) (sig : Nat) (exp : Int) : FloatDatum :=
  if sig = 0 then .num neg 0 0 else canonAux sig neg sig exp

/-- A datum of `𝔽_w`: canonical, and — for a finite one — representable at the
width. `sig < 2^p` is the significand bound, `eMin ≤ exp` the subnormal
floor, and `sig < 2^(eTop - exp)` says the magnitude stays below `2^eTop`,
which is the finite range. The specials belong to every width. This is the
float counterpart of `InBounds` (`Syntax.lean`): the side condition §6.1
states on a value form, and the property §7's totality lemma says every §6.4
float rule preserves. -/
def FloatDatum.Wf (w : FloatWidth) : FloatDatum → Prop
  | .nan _ => True
  | .inf _ => True
  | .num _ sig exp =>
      (sig = 0 ∧ exp = 0) ∨
        (sig % 2 = 1 ∧ sig < 2 ^ w.prec ∧ w.eMin ≤ exp ∧ exp ≤ w.eTop ∧
          sig < 2 ^ (w.eTop - exp).toNat)

instance (w : FloatWidth) (f : FloatDatum) : Decidable (f.Wf w) := by
  unfold FloatDatum.Wf; split <;> infer_instance

/-- `±0`, the zero of the given sign (`3.12:1`: the two are distinct
values). -/
def FloatDatum.zero (neg : Bool) : FloatDatum := .num neg 0 0

/-- Whether the datum is a NaN (`3.12:27`'s unordered case) (helper). -/
def FloatDatum.isNaN : FloatDatum → Bool
  | .nan _ => true
  | _ => false

/-- `(D-Float-Neg)` §6.4: negation is a **sign flip and nothing else**, on
`-0.0` and on a NaN alike (`3.12:24`). It is exact, so it is a function here
rather than a field of the model, and it is total — no float redex steps to a
panic. -/
def FloatDatum.negate : FloatDatum → FloatDatum
  | .nan b => .nan (!b)
  | .inf b => .inf (!b)
  | .num b sig exp => .num (!b) sig exp

/-- The widening half of `(D-Float-Cast)`: exact, hence the identity on the
datum (`3.12:19`: widening `f32` to `f64` is value-preserving). -/
def FloatDatum.widen (f : FloatDatum) : FloatDatum := f

/-! ## Exact comparison of two finite magnitudes

Two canonical significand/exponent pairs are compared by cross-scaling into
`Nat`, so the order below is the mathematical one on `sig · 2^exp` and no
rounding enters it. -/

/-- `a · 2^i` compared with `b · 2^j`, as `-1 / 0 / 1`, by scaling both to the
smaller exponent (helper). -/
def cmpScaled (a : Nat) (i : Int) (b : Nat) (j : Int) : Int :=
  match compare (a * 2 ^ (i - min i j).toNat) (b * 2 ^ (j - min i j).toNat) with
  | .lt => -1
  | .eq => 0
  | .gt => 1

/-- An exact magnitude comparison is one of `-1`, `0`, `1` (helper). -/
theorem cmpScaled_trichotomy (a : Nat) (i : Int) (b : Nat) (j : Int) :
    cmpScaled a i b j = -1 ∨ cmpScaled a i b j = 0 ∨ cmpScaled a i b j = 1 := by
  unfold cmpScaled
  cases compare (a * 2 ^ (i - min i j).toNat) (b * 2 ^ (j - min i j).toNat)
  · exact Or.inl rfl
  · exact Or.inr (Or.inl rfl)
  · exact Or.inr (Or.inr rfl)

/-- The magnitudes of two finite data compared, ignoring their signs
(helper). -/
def magCmp (s₁ : Nat) (e₁ : Int) (s₂ : Nat) (e₂ : Int) : Int := cmpScaled s₁ e₁ s₂ e₂

/-- A magnitude comparison is one of `-1`, `0`, `1` (helper). -/
theorem magCmp_trichotomy (s₁ : Nat) (e₁ : Int) (s₂ : Nat) (e₂ : Int) :
    magCmp s₁ e₁ s₂ e₂ = -1 ∨ magCmp s₁ e₁ s₂ e₂ = 0 ∨ magCmp s₁ e₁ s₂ e₂ = 1 :=
  cmpScaled_trichotomy s₁ e₁ s₂ e₂

/-- Reversing a trichotomous comparison keeps it trichotomous, which is what
the negative half of `≺_w` does (helper). -/
theorem negIf_trichotomy {c : Int} (h : c = -1 ∨ c = 0 ∨ c = 1) (n : Bool) :
    (if n then -c else c) = -1 ∨ (if n then -c else c) = 0 ∨ (if n then -c else c) = 1 := by
  rcases h with h | h | h <;> cases n <;> simp [h]

/-! ## §6.4's ordering compares and `@total_cmp` -/

/-- `(D-Float-Ord)` §6.4 at `<`: the IEEE 754 predicate. A NaN operand makes
the two **unordered**, so every ordering compare is `false` (`3.12:27`), and
`-0.0` and `+0.0` compare equal, neither below the other (`3.12:28`). -/
def FloatDatum.lt : FloatDatum → FloatDatum → Bool
  | .nan _, _ => false
  | _, .nan _ => false
  | .inf true, .inf true => false
  | .inf true, _ => true
  | _, .inf true => false
  | .inf false, _ => false
  | _, .inf false => true
  | .num n₁ s₁ e₁, .num n₂ s₂ e₂ =>
      if s₁ = 0 && s₂ = 0 then false                      -- ±0 are equal (3.12:28)
      else if s₁ = 0 then !n₂                             -- 0 < positive
      else if s₂ = 0 then n₁                              -- negative < 0
      else if n₁ && !n₂ then true
      else if !n₁ && n₂ then false
      else if n₁ then magCmp s₂ e₂ s₁ e₁ = -1             -- both negative: reversed
      else magCmp s₁ e₁ s₂ e₂ = -1

/-- `(D-Float-Ord)` at `<=`: `a <= b` is `a < b` or the two are IEEE-equal,
and a NaN operand still yields `false` (`3.12:27`). -/
def FloatDatum.le (a b : FloatDatum) : Bool :=
  if a.isNaN || b.isNaN then false else !b.lt a

/-- `≺_w`, the IEEE 754 `totalOrder` predicate of §6.4, as a rank: every
negative NaN, then `-inf`, the negative finite values, `-0.0`, `+0.0`, the
positive finite values, `+inf`, then every positive NaN (`3.12:32`). Within a
sign the finite values are compared by magnitude, reversed on the negative
side (helper). -/
def FloatDatum.totalRank : FloatDatum → Int
  | .nan true => -3
  | .inf true => -2
  | .num true _ _ => -1
  | .num false _ _ => 1
  | .inf false => 2
  | .nan false => 3

/-- `(D-Total-Cmp)` §6.4: `-1`, `0`, `1` under `≺_w`. `k = 0` holds exactly
when the two operands are the **same datum** — which is why `FloatDatum` is
kept canonical — and that is total precisely where `≈` is not:
`totalCmp f f = 0` for every `f`, a NaN included. `3.12:32` fixes only the
*sign* of a non-zero result; the implementations return `-1`/`0`/`1`
(verified against the compiler), and the model returns the same so that a
printed `@total_cmp` has a comparable value. -/
def FloatDatum.totalCmp (a b : FloatDatum) : Int :=
  match compare a.totalRank b.totalRank with
  | .lt => -1
  | .gt => 1
  | .eq =>
      match a, b with
      | .num n s₁ e₁, .num _ s₂ e₂ =>
          if n then -(magCmp s₁ e₁ s₂ e₂) else magCmp s₁ e₁ s₂ e₂
      | _, _ => 0

/-- `(D-Total-Cmp)` yields one of `-1`, `0`, `1` — which is what makes its
result a value of `int(32, signed)` whatever the operands are. `3.12:32` fixes
only the *sign*; this is the witness the implementations agree on. -/
theorem totalCmp_trichotomy (a b : FloatDatum) :
    a.totalCmp b = -1 ∨ a.totalCmp b = 0 ∨ a.totalCmp b = 1 := by
  unfold FloatDatum.totalCmp
  split
  · exact Or.inl rfl
  · exact Or.inr (Or.inr rfl)
  · split
    · exact negIf_trichotomy (magCmp_trichotomy _ _ _ _) _
    · exact Or.inr (Or.inl rfl)

/-! ## §6.4's exact conversions -/

/-- The datum truncated toward zero, as an exact integer: `none` for a NaN and
for `±inf`, which is the half of `(D-Float-To-Int)`'s premise that has nothing
to do with the target's range (helper). -/
def FloatDatum.truncToInt : FloatDatum → Option Int
  | .nan _ => none
  | .inf _ => none
  | .num neg sig exp =>
      let mag : Nat :=
        if 0 ≤ exp then sig * 2 ^ exp.toNat else sig / 2 ^ (-exp).toNat
      some (if neg then -(mag : Int) else (mag : Int))

/-- The five `(Float-Round)` intrinsics of `3.12:34`, minus `@sqrt`: rounding
toward `-inf`, toward `+inf`, toward zero, and to nearest with ties **away**
from zero (`3.12:36`). Each is exact, none traps (`3.12:37`), and a NaN, an
infinity, and a zero each come back unchanged (a zero keeps its sign). -/
inductive FloatRoundOp where
  | floor
  | ceil
  | trunc
  | round
deriving DecidableEq, Repr

/-- Add one to a finite datum's magnitude, staying in `𝔽_w`'s integral values
(helper). -/
def FloatDatum.succMag (neg : Bool) (m : Nat) : FloatDatum := canonNum neg (m + 1) 0

/-- `(D-Float-Round)` §6.4 for the four exact intrinsics (`3.12:36`). A datum
with no fractional part — an infinity, a NaN, a zero, or any value whose
exponent is non-negative — comes back unchanged; otherwise the integral part
`m` and the remainder `r` decide, and the value is below `2^p`, so `m` and
`m + 1` are both representable. -/
def FloatDatum.roundOp (op : FloatRoundOp) (f : FloatDatum) : FloatDatum :=
  match f with
  | .nan b => .nan b
  | .inf b => .inf b
  | .num neg sig exp =>
      if 0 ≤ exp then .num neg sig exp
      else
        let d : Nat := 2 ^ (-exp).toNat
        let m : Nat := sig / d
        let r : Nat := sig % d
        if r = 0 then .num neg sig exp
        else
          match op with
          | .trunc => canonNum neg m 0
          | .floor => if neg then FloatDatum.succMag true m else canonNum false m 0
          | .ceil => if neg then canonNum true m 0 else FloatDatum.succMag false m
          | .round => if 2 * r ≥ d then FloatDatum.succMag neg m else canonNum neg m 0

/-! ## Closure of the exact operations in `𝔽_w`

§7's totality lemma asks that every §6.4 float operation land *in* `𝔽_w`. For
the operations this module defines — the sign flip, the widening cast, and the
four exact rounding intrinsics — that is a theorem, proved here. For the
rounded ones it is a law of `FloatModel`, because rounding is IEEE's and not
this module's. -/

/-- The canonicalization loop lands in `𝔽_w` whenever the number it is handed
does: stripping a factor of two halves the significand and raises the
exponent, which preserves every clause of `Wf` (helper). -/
theorem canonAux_ok :
    ∀ (k : Nat) (neg : Bool) (sig : Nat) (exp : Int) (P : Nat) (L T : Int),
      sig ≠ 0 → sig < 2 ^ k → sig < 2 ^ P → L ≤ exp → exp ≤ T →
      sig < 2 ^ (T - exp).toNat →
      ∃ s' e', canonAux k neg sig exp = .num neg s' e' ∧ s' % 2 = 1 ∧ s' < 2 ^ P ∧
        L ≤ e' ∧ e' ≤ T ∧ s' < 2 ^ (T - e').toNat
  | 0, _, sig, _, _, _, _, hne, hk, _, _, _, _ =>
      -- `sig < 2^0 = 1` and `sig ≠ 0` cannot both hold. Written as an
      -- explicit `absurd` rather than left to `omega`: `omega` closing a
      -- goal that is not itself arithmetic reaches for `Classical.choice`,
      -- which this package does not allow itself (`TRUST.md`).
      absurd (Nat.lt_one_iff.mp (by rw [Nat.pow_zero] at hk; exact hk)) hne
  | k + 1, neg, sig, exp, P, L, T, hne, hk, hp, hlo, hhi, htop => by
      simp only [canonAux]
      split
      · next heven =>
          have hpow : (2 : Nat) ^ (k + 1) = 2 ^ k * 2 := by rw [Nat.pow_succ]
          have hT : (T - exp).toNat ≠ 0 := by
            intro hz; rw [hz, Nat.pow_zero] at htop; omega
          have hs : (T - exp).toNat = (T - (exp + 1)).toNat + 1 := by omega
          have hpow2 : (2 : Nat) ^ (T - exp).toNat = 2 ^ (T - (exp + 1)).toNat * 2 := by
            rw [hs, Nat.pow_succ]
          exact canonAux_ok k neg (sig / 2) (exp + 1) P L T (by omega) (by omega) (by omega)
            (by omega) (by omega) (by omega)
      · next hodd => exact ⟨sig, exp, rfl, by omega, hp, hlo, hhi, htop⟩

/-- `canonNum` lands in `𝔽_w` whenever the number it is handed does
(helper). -/
theorem canonNum_wf {w : FloatWidth} {neg : Bool} {sig : Nat} {exp : Int}
    (hp : sig < 2 ^ w.prec) (hlo : w.eMin ≤ exp) (hhi : exp ≤ w.eTop)
    (htop : sig < 2 ^ (w.eTop - exp).toNat) : (canonNum neg sig exp).Wf w := by
  unfold canonNum
  split
  · exact Or.inl ⟨rfl, rfl⟩
  · next hne =>
      obtain ⟨s', e', heq, h1, h2, h3, h4, h5⟩ :=
        canonAux_ok sig neg sig exp w.prec w.eMin w.eTop hne Nat.lt_two_pow_self hp hlo hhi htop
      rw [heq]
      exact Or.inr ⟨h1, h2, h3, h4, h5⟩

/-- `1 · 2^0` is a datum of every width — the one finite value the trap
witnesses need to name (helper). -/
theorem one_wf (w : FloatWidth) : (FloatDatum.num false 1 0).Wf w := by
  refine Or.inr ⟨rfl, ?_, ?_, ?_, ?_⟩
  · exact Nat.one_lt_two_pow_iff.mpr (by cases w <;> decide)
  · cases w <;> decide
  · cases w <;> decide
  · exact Nat.one_lt_two_pow_iff.mpr (by cases w <;> decide)

/-- **`neg` preserves `𝔽_w`** — it flips a sign bit and changes nothing else
(`3.12:24`), so §7's totality obligation for `(D-Float-Neg)` is discharged
here rather than assumed. -/
theorem negate_wf {w : FloatWidth} {f : FloatDatum} (h : f.Wf w) : f.negate.Wf w := by
  cases f <;> simp_all [FloatDatum.negate, FloatDatum.Wf]

/-- **Widening preserves representability** (`3.12:19`: widening `f32` to
`f64` is value-preserving), which is why the widening half of
`(D-Float-Cast)` is the identity on the datum and needs no law. -/
theorem widen_wf {f : FloatDatum} (h : f.Wf .w32) : f.widen.Wf .w64 := by
  cases f with
  | nan _ => trivial
  | inf _ => trivial
  | num n sig exp =>
      simp only [FloatDatum.widen, FloatDatum.Wf, FloatWidth.prec, FloatWidth.eMin,
        FloatWidth.eTop] at h ⊢
      rcases h with ⟨h0, he⟩ | ⟨hodd, hp, hlo, hhi, htop⟩
      · exact Or.inl ⟨h0, he⟩
      · refine Or.inr ⟨hodd, ?_, by omega, by omega, ?_⟩
        · exact Nat.lt_of_lt_of_le hp (Nat.pow_le_pow_right (by decide) (by decide))
        · exact Nat.lt_of_lt_of_le htop (Nat.pow_le_pow_right (by decide) (by omega))

/-- **The four exact rounding intrinsics preserve `𝔽_w`** (`3.12:36`: "each
result is exact — an integral value near `x` is always representable"). With
`negate_wf` and `widen_wf` this discharges §7's "each `⊙_w` of §6.4 is total
on `𝔽_w`" for every float operation this module defines; `@sqrt`, the one that
rounds, is `FloatModel.sqrt_wf`. -/
theorem roundOp_wf {w : FloatWidth} {op : FloatRoundOp} {f : FloatDatum} (h : f.Wf w) :
    (f.roundOp op).Wf w := by
  cases f with
  | nan _ => trivial
  | inf _ => trivial
  | num n sig exp =>
      simp only [FloatDatum.roundOp]
      split
      · exact h
      · next hexp =>
          split
          · exact h
          · next hr =>
              rcases h with ⟨h0, _⟩ | ⟨hodd, hp, hlo, hhi, htop⟩
              · exact absurd (by simp [h0]) hr
              · -- `exp < 0`, so the integral part is below `2^(p-1)`: it and
                -- its successor are both representable at exponent `0`.
                have hprec : 2 ^ w.prec = 2 ^ (w.prec - 1) * 2 := by
                  rw [← Nat.pow_succ]; congr 1; cases w <;> decide
                have hpos : 0 < 2 ^ (w.prec - 1) := Nat.two_pow_pos _
                have hd2 : 2 ≤ 2 ^ (-exp).toNat := by
                  have h1 : 1 ≤ (-exp).toNat := by omega
                  calc (2 : Nat) = 2 ^ 1 := rfl
                    _ ≤ 2 ^ (-exp).toNat := Nat.pow_le_pow_right (by decide) h1
                have hm : sig / 2 ^ (-exp).toNat ≤ sig / 2 :=
                  Nat.div_le_div_left hd2 (by decide)
                have hmlt : sig / 2 ^ (-exp).toNat + 1 < 2 ^ w.prec := by omega
                have hmlt0 : sig / 2 ^ (-exp).toNat < 2 ^ w.prec := by omega
                have hemin : w.eMin ≤ (0 : Int) := by cases w <;> decide
                have hetop : (0 : Int) ≤ w.eTop := by cases w <;> decide
                have hbig : 2 ^ w.prec ≤ 2 ^ (w.eTop - (0 : Int)).toNat :=
                  Nat.pow_le_pow_right (by decide) (by cases w <;> decide)
                have hsucc : ∀ b : Bool,
                    (FloatDatum.succMag b (sig / 2 ^ (-exp).toNat)).Wf w :=
                  fun b => canonNum_wf hmlt hemin hetop (by omega)
                have hplain : ∀ b : Bool,
                    (canonNum b (sig / 2 ^ (-exp).toNat) 0).Wf w :=
                  fun b => canonNum_wf hmlt0 hemin hetop (by omega)
                cases op <;> dsimp only <;>
                  first
                    | exact hplain _
                    | (split <;> first | exact hsucc _ | exact hplain _)

/-! ## Rounding an exact value into `𝔽_w` (`rnd_w`)

§2's second model parameter is `rnd_w`, round to nearest with ties to even —
the IEEE 754 default attribute, which `3.12:9` fixes for literals and
`3.12:21` for arithmetic. Rue has no dynamic rounding mode, so no rule takes
one. The function below is that rounding, computed exactly on a rational
`num/den`: it is what `Float.exactOps` builds every rounded operation out of,
and it is constructive, so no host float and no axiom enters the package. -/

/-- Round `a / b` to a natural number, to nearest with ties to even
(helper). -/
def roundDivHalfEven (a b : Nat) : Nat :=
  let q := a / b
  let r := a % b
  if 2 * r < b then q else if b < 2 * r then q + 1 else if q % 2 = 0 then q else q + 1

/-- `⌊log₂ (num/den)⌋` for a positive rational, exactly. `bitLen num - bitLen
den` is within one of the answer, and one comparison settles it (helper). -/
def log2Floor (num den : Nat) : Int :=
  let g : Int := (bitLen num : Int) - (bitLen den : Int)
  -- `num/den ≥ 2^g` decides between `g` and `g - 1`.
  let lhs := num * 2 ^ (max 0 (-g)).toNat
  let rhs := den * 2 ^ (max 0 g).toNat
  if rhs ≤ lhs then g else g - 1

/-- `rnd_w` of the exact rational `(-1)^neg · num/den`: the nearest datum of
`𝔽_w`, ties to even, with an exact zero (and an underflowing one) giving the
zero of the sign asked for and an overflowing magnitude giving `±inf`
(`3.12:23`: "the exact result too large for `𝔽_w` yields `±inf` of the exact
result's sign"). `den` is assumed non-zero by every caller. -/
def roundRat (w : FloatWidth) (neg : Bool) (num den : Nat) : FloatDatum :=
  if num = 0 then .num neg 0 0
  else
    let e0 := log2Floor num den
    let e := max w.eMin (e0 - (w.prec : Int) + 1)
    -- `num/den / 2^e`, as one fraction.
    let n' := num * 2 ^ (max 0 (-e)).toNat
    let d' := den * 2 ^ (max 0 e).toNat
    let m := roundDivHalfEven n' d'
    -- Rounding up from just below `2^p` lands on `2^p`; renormalize once.
    let (m, e) := if m = 2 ^ w.prec then (2 ^ (w.prec - 1), e + 1) else (m, e)
    if m = 0 then .num neg 0 0
    else if w.eTop < e ∨ 2 ^ (w.eTop - e).toNat ≤ m then .inf neg
    else canonNum neg m e

/-! ## The rounded operations the model supplies -/

/-- §6.4's four float arithmetic operators (`(D-Float-Arith)`). They are
listed apart from `BinOp` because the model's field is indexed by them and
because `%` is **not** among them: `(Float-Arith)` §5.8 omits it and
`3.12:25` says why. -/
inductive FloatArith where
  | add
  | sub
  | mul
  | div
deriving DecidableEq, Repr

/-- The operations of §6.4 whose result is `rnd_w` of a value that need not lie
in `𝔽_w`, and the target parameter `σ_NaN` they *create* NaNs at. §2 fixes both
per target rather than per rule, so they are the interface the development is
parameterized over; every exact operation is a function of this module
instead. -/
structure FloatOps where
  /-- `(D-Float-Arith)`: `f₁ ⊕_w f₂`, the exact mathematical result rounded by
  `rnd_w`, with IEEE's special cases for zero, infinite and NaN operands
  (`3.12:21`, `3.12:22`, `3.12:23`). Total: no float arithmetic redex steps to
  a panic. -/
  arith : FloatWidth → FloatArith → FloatDatum → FloatDatum → FloatDatum
  /-- `(D-Float-Round)` for `@sqrt`: the IEEE 754 square root, correctly
  rounded (`3.12:35`). -/
  sqrt : FloatWidth → FloatDatum → FloatDatum
  /-- `rnd_w` of a decimal literal (`3.12:9`). Elaboration resolves the
  literal's type before the core, so the width is the one the form carries. -/
  ofLit : FloatWidth → Nat → Bool → Nat → FloatDatum
  /-- `(D-Int-To-Float)`: `rnd_w` of an exact integer value (`3.12:16`). -/
  ofInt : FloatWidth → Int → FloatDatum
  /-- The narrowing half of `(D-Float-Cast)`: `rnd_32` of an `f64` datum
  (`3.12:19`). The widening half is exact and is `FloatDatum.widen`. -/
  narrow : FloatDatum → FloatDatum
  /-- `σ_NaN` (§2, `3.12:44`): the sign of a NaN the target's hardware
  produces — negative on x86-64, positive on AArch64 (Appendix B.1). It is the
  sign *bit*, the same `Bool` `FloatDatum.nan` carries, so `false` is the
  AArch64 (positive) choice and `true` the x86-64 (negative) one. It is the
  sign of a NaN an operation **creates** — `0/0`, `inf - inf`, `0 · inf`,
  `inf/inf`, `@sqrt` of a negative. A NaN that merely passes *through* an
  operation is propagated with its own sign on both targets, so `σ_NaN` does
  not reach it (`FloatModel.arith_nan`, `Float.addD`). -/
  nanSign : Bool

/-- `(D-Float-Cast)` §6.4 at either direction, given the model's narrowing.
`(Float-Cast)` §5.8 imposes `w' ≠ w`, so the equal-width case is unreachable
from a well-typed program and is the identity here. -/
def FloatOps.cast (M : FloatOps) (w w' : FloatWidth) (f : FloatDatum) : FloatDatum :=
  match w, w' with
  | .w64, .w32 => M.narrow f
  | .w32, .w64 => f.widen
  | _, _ => f

/-- `(D-Float-Round)` §6.4 over all five intrinsics of `3.12:34`: `@sqrt` is
the model's, the other four are exact. -/
inductive FloatUnIntrin where
  | sqrt
  | round (op : FloatRoundOp)
deriving DecidableEq, Repr

/-- The five `3.12:34` intrinsics applied (`(D-Float-Round)`). None traps
(`3.12:37`). -/
def FloatOps.roundIntrin (M : FloatOps) (w : FloatWidth) :
    FloatUnIntrin → FloatDatum → FloatDatum
  | .sqrt, f => M.sqrt w f
  | .round op, f => f.roundOp op

/-! ## The laws — §7's "totality of the float operations", named

§7 owes one lemma for floats: *"For each `w ∈ {32, 64}`: `⊕_w` is a total
function `𝔽_w × 𝔽_w → 𝔽_w`, `rnd_w` is total into `𝔽_w`, `≺_w` is a total
order on `𝔽_w`, each `⊙_w` of §6.4 is total on `𝔽_w`, and the premises of
`(D-Float-To-Int)` and `(D-Float-To-Int-Trap)` partition `𝔽_w`. This is what
makes every float redex able to step… It is a statement about IEEE 754,
discharged against the standard rather than against Rue."*

Mechanized, that lemma splits three ways.

* **Totality as a Lean function** is free: each operation above is a total
  function, so no float redex is stuck for want of a result. Nothing is
  assumed.
* **The partition** is a theorem here (`floatToInt_partition`), not an
  assumption, because the datum model makes truncation exact integer
  arithmetic.
* **Closure in `𝔽_w`** — that `rnd_w` and each rounded operation land *in*
  `𝔽_w` rather than on some datum outside it — is what cannot be proved of an
  arbitrary `FloatOps`, and it is the content of the fields below. It is the
  float counterpart of `valOf_inBounds` (`Syntax.lean`), which *is* proved,
  because `val_{w,s}` is arithmetic and `rnd_w` is IEEE.

Behavioural laws join them, quoted from §6.4's own "spelled out, as
consequences of `⊕_w`" list: they are what a *witness* for the one float trap
rests on, so that no witness has to compute with a concrete model.

The two NaN laws are deliberately the **weak** ones: a NaN operand makes the
result *a* NaN, and nothing is assumed about which NaN. IEEE 754 guarantees no
more than that, and neither does any target Rue has: x86-64 and AArch64 both
*propagate* a NaN operand, sign and all, and reserve `σ_NaN` for a NaN an
invalid operation **creates**. A law that also fixed the sign of a propagated
NaN would be false of the standard and false of the compiler — so the sign of
a propagated NaN is a *model* choice, made in `Float.exactOps` (which
propagates the first NaN operand's sign, as both targets do) and checked
against the compiler case by case, never a theorem here. §9 item 5 (RUE-2283)
is where the target-defined part is tracked. -/

/-- **§7's "totality of the float operations", as an interface.** A
`FloatOps` together with the laws §7 owes for floats and §6.4 quotes from
`3.12:9`, `3.12:22` and `3.12:44`. Every field is a statement that is true of
IEEE 754 *and* of the compiler — which is why the NaN laws below say only that
a NaN comes out, and leave its sign to the model (see the section note above).
They are *fields* rather than `axiom` declarations so that every theorem
resting on one carries it in its own statement (`TRUST.md`, "Assumptions
carried as interfaces"). §7 says the lemma is "discharged against the standard
rather than against Rue"; this is that sentence, mechanized. -/
structure FloatModel extends FloatOps where
  /-- **Closure of `⊕_w`** (§7): the four arithmetic operators map `𝔽_w × 𝔽_w`
  into `𝔽_w`. With Lean totality this is §7's "`⊕_w` is a total function
  `𝔽_w × 𝔽_w → 𝔽_w`". -/
  arith_wf : ∀ w op a b, a.Wf w → b.Wf w → (toFloatOps.arith w op a b).Wf w
  /-- **Closure of `@sqrt`** (§7's "each `⊙_w` is total on `𝔽_w`"; the other
  four `⊙_w` are exact and proved closed here). -/
  sqrt_wf : ∀ w f, f.Wf w → (toFloatOps.sqrt w f).Wf w
  /-- **Closure of `rnd_w` on a literal** (`3.12:9`, and §7's "`rnd_w` is
  total into `𝔽_w`"). -/
  ofLit_wf : ∀ w m ne e, (toFloatOps.ofLit w m ne e).Wf w
  /-- **Closure of `rnd_w` on an integer** (`(D-Int-To-Float)`, `3.12:16`). -/
  ofInt_wf : ∀ w n, (toFloatOps.ofInt w n).Wf w
  /-- **Closure of the narrowing cast** (`(D-Float-Cast)`, `3.12:19`). -/
  narrow_wf : ∀ f, f.Wf .w64 → (toFloatOps.narrow f).Wf .w32
  /-- **A NaN operand yields a NaN** (IEEE 754, and §6.4 lists it among the
  consequences of `⊕_w`). The *sign* is deliberately left open: IEEE 754 says
  only that a NaN comes out, both of Rue's targets propagate the operand's
  sign rather than substituting `σ_NaN`, and `3.12:44` fixes `σ_NaN` for a NaN
  an invalid operation *creates* — which `zero_div_zero` below is. -/
  arith_nan : ∀ w op a b, (a.isNaN = true ∨ b.isNaN = true) →
    (toFloatOps.arith w op a b).isNaN = true
  /-- **A cast of a NaN is a NaN** (`(D-Float-Cast)`, `3.12:19`): the sibling
  of `arith_nan` at the narrowing half, again with the sign left open. The
  widening half needs no law — it is `FloatDatum.widen`, the identity
  (`cast_nan`). -/
  narrow_nan : ∀ f, f.isNaN = true → (toFloatOps.narrow f).isNaN = true
  /-- **A finite non-zero divided by a zero is the infinity of the xor sign**
  (`3.12:22`, quoted in §6.4). This is the law a `@float_to_int` trap witness
  rests on: it is how a core program reaches an infinity at all. -/
  div_by_zero : ∀ w a n s e, a.Wf w → a = .num n s e → s ≠ 0 →
    ∀ n₂, toFloatOps.arith w .div a (.num n₂ 0 0) = .inf (xor n n₂)
  /-- **A zero divided by a zero is `NaN(σ_NaN)`** (`3.12:22`, quoted in
  §6.4). -/
  zero_div_zero : ∀ w n₁ n₂,
    toFloatOps.arith w .div (.num n₁ 0 0) (.num n₂ 0 0) = .nan toFloatOps.nanSign
  /-- **The decimal zero is `+0`** — `3.12:9`'s last sentence, "a literal that
  is representable in the target type denotes exactly that value", at the one
  literal every width represents. -/
  ofLit_zero : ∀ w ne e, toFloatOps.ofLit w 0 ne e = .num false 0 0
  /-- **The decimal one is `1 · 2^0`** — the same sentence of `3.12:9` at the
  other literal this slice's witnesses need. -/
  ofLit_one : ∀ w, toFloatOps.ofLit w 1 false 0 = .num false 1 0

/-- **`@float_cast` of a NaN is a NaN, in either direction** —
`(D-Float-Cast)` §6.4 on a special (`3.12:19`). The narrowing half is
`narrow_nan`; the widening half is `FloatDatum.widen`, the identity, so it is
*proved* rather than assumed. As with `arith_nan`, the sign is not fixed: both
targets keep the operand's. -/
theorem FloatModel.cast_nan (M : FloatModel) (w w' : FloatWidth) {f : FloatDatum}
    (h : f.isNaN = true) : (M.toFloatOps.cast w w' f).isNaN = true := by
  cases w <;> cases w' <;>
    simp only [FloatOps.cast, FloatDatum.widen] <;>
    first
      | exact h
      | exact M.narrow_nan f h

/-! ## The partition `(D-Float-To-Int)` / `(D-Float-To-Int-Trap)` -/

/-- `(D-Float-To-Int)` §6.4: the operand truncated toward zero when that is
defined and lands in the target's range, and `none` — which the machine turns
into `↯overflow` — when it does not. `3.12:18` states the guard both ways and
§6.4 notes the two readings agree, with both infinities failing under either;
this is the "`f` is neither a NaN nor `±inf`, and `t ∈ [min_{T'}, max_{T'}]`"
reading. -/
def FloatDatum.toIntIn (f : FloatDatum) (lo hi : Int) : Option Int :=
  match f.truncToInt with
  | none => none
  | some t => if lo ≤ t ∧ t ≤ hi then some t else none

/-- **The two premises partition `𝔽_w`** — §7's obligation for the one float
form that traps, and a *theorem* here rather than an assumption, because the
datum model makes truncation exact integer arithmetic. Progress for
`@float_to_int` is exactly this: every datum either converts or traps, and
never both. -/
theorem floatToInt_partition (f : FloatDatum) (lo hi : Int) :
    (∃ t, f.toIntIn lo hi = some t ∧ lo ≤ t ∧ t ≤ hi) ∨ f.toIntIn lo hi = none := by
  unfold FloatDatum.toIntIn
  split
  · exact Or.inr rfl
  · next t _ =>
      by_cases h : lo ≤ t ∧ t ≤ hi
      · exact Or.inl ⟨t, by rw [if_pos h], h.1, h.2⟩
      · exact Or.inr (by rw [if_neg h])

/-- A converted value lies in the target's range, which is the half of
`(D-Float-To-Int)`'s premise the result type needs (helper). -/
theorem toIntIn_mem {f : FloatDatum} {lo hi t : Int} (h : f.toIntIn lo hi = some t) :
    lo ≤ t ∧ t ≤ hi := by
  unfold FloatDatum.toIntIn at h
  split at h
  · cases h
  · next t' _ =>
      by_cases hc : lo ≤ t' ∧ t' ≤ hi
      · rw [if_pos hc] at h; cases h; exact hc
      · rw [if_neg hc] at h; cases h

/-- A NaN never converts: the first half of `(D-Float-To-Int-Trap)`'s premise
(`3.12:18`). -/
theorem toIntIn_nan (b : Bool) (lo hi : Int) : (FloatDatum.nan b).toIntIn lo hi = none := rfl

/-- An infinity never converts: `3.12:18`'s "admits both infinities as
failures". This and `toIntIn_nan` are what a float trap witness appeals to. -/
theorem toIntIn_inf (b : Bool) (lo hi : Int) : (FloatDatum.inf b).toIntIn lo hi = none := rfl

/-! ## `3.12:40`–`3.12:42`: the rendering

The text `@dbg` produces for a float. §6.4 fixes no rendering — the calculus
says so explicitly — so this is the *spec's* rule, and the compiler realizes
the same paragraphs through the vendored `zmij` shortest-round-trip formatter
(`crates/rue-runtime/src/string.rs`). Every corpus case's expected stdout is
checked against the compiler case by case, which is what keeps the two
honest. -/

/-- The decimal digits of a natural number, most significant first, on a
digit budget (helper). -/
def digitsAux : Nat → Nat → List Char
  | 0, n => [Char.ofNat (48 + n % 10)]
  | k + 1, n =>
      if n < 10 then [Char.ofNat (48 + n)]
      else digitsAux k (n / 10) ++ [Char.ofNat (48 + n % 10)]

/-- The decimal digits of a natural number, most significant first. The budget
is `bitLen n`, which is at least its decimal length (helper). -/
def digitsOf (n : Nat) : List Char := digitsAux (bitLen n + 1) n

/-- `n` as a decimal string (helper). -/
def natDecimal (n : Nat) : String := String.ofList (digitsOf n)

/-- Drop the trailing zeros of a decimal digit list (helper). -/
def stripTrailingZeros (ds : List Char) : List Char :=
  let zeros := (ds.reverse.takeWhile (· = '0')).length
  ds.take (ds.length - zeros)

/-- The rounding interval of a finite non-zero canonical datum, as the triple
`(lo, hi, closed)` of `Nat`s scaled by a common `2^t`: a decimal round-trips
to this datum exactly when it lies between `lo` and `hi`, inclusively when the
IEEE significand is even (`rnd_w` is ties-to-even, §2). The gap above is one
ulp and the gap below is the same unless the datum is a power of two with a
predecessor at the smaller exponent, where it is half — the classic asymmetry
(helper). -/
def roundInterval (w : FloatWidth) (sig : Nat) (exp : Int) : Nat × Nat × Int × Bool :=
  let k : Int := (bitLen sig : Int)
  let eUlp : Int := max (exp + k - (w.prec : Int)) w.eMin
  let tighter : Bool := sig = 1 && w.eMin < exp + k - (w.prec : Int)
  let t : Int := min exp (eUlp - 2)
  let v : Nat := sig * 2 ^ (exp - t).toNat
  let halfUlp : Nat := 2 ^ (eUlp - 1 - t).toNat
  let lowGap : Nat := if tighter then 2 ^ (eUlp - 2 - t).toNat else halfUlp
  let closed : Bool := exp > eUlp        -- the significand is even (`sig` is odd)
  (v - lowGap, v + halfUlp, t, closed)

/-- `d · 10^n` compared with `b · 2^t`, exactly: `10^n = 2^n · 5^n`, so both
sides clear to naturals (helper). -/
def cmpDecBin (d : Nat) (n : Int) (b : Nat) (t : Int) : Ordering :=
  let shift : Int := n - t
  let lhs : Nat := d * 5 ^ (max 0 n).toNat * 2 ^ (max 0 shift).toNat
  let rhs : Nat := b * 5 ^ (max 0 (-n)).toNat * 2 ^ (max 0 (-shift)).toNat
  compare lhs rhs

/-- Whether the decimal `d · 10^n` lies in the rounding interval of
`sig · 2^exp` at width `w`, i.e. whether reading it back at that width
recovers the datum (helper). -/
def roundTrips (w : FloatWidth) (sig : Nat) (exp : Int) (d : Nat) (n : Int) : Bool :=
  let (lo, hi, t, closed) := roundInterval w sig exp
  let cmpLo := cmpDecBin d n lo t
  let cmpHi := cmpDecBin d n hi t
  let aboveLo := if closed then cmpLo != Ordering.lt else cmpLo == Ordering.gt
  let belowHi := if closed then cmpHi != Ordering.gt else cmpHi == Ordering.lt
  aboveLo && belowHi

/-- The `L`-significant-digit decimal nearest `sig · 2^exp` at scale `10^n`,
rounded to nearest with ties to even (helper). -/
def nearestDigits (sig : Nat) (exp : Int) (n : Int) : Nat :=
  -- `sig · 2^exp / 10^n` as one fraction, then round.
  let num := sig * 2 ^ (max 0 exp).toNat * 5 ^ (max 0 (-n)).toNat * 2 ^ (max 0 (-n)).toNat
  let den := 2 ^ (max 0 (-exp)).toNat * 5 ^ (max 0 n).toNat * 2 ^ (max 0 n).toNat
  roundDivHalfEven num den

/-- Whether `sig · 2^exp ≥ 10^e`, exactly (helper). -/
def gePow10 (sig : Nat) (exp : Int) (e : Int) : Bool :=
  cmpDecBin 1 e sig exp != Ordering.gt

/-- `⌊log₁₀ v⌋` for `v = sig · 2^exp > 0`, exactly. The binary estimate
`(bitLen sig + exp - 1) · log₁₀ 2` is within a step or two, and the walk below
closes the gap on a fixed budget (helper). -/
def log10Up : Nat → Nat → Int → Int → Int
  | 0, _, _, e => e
  | k + 1, sig, exp, e => if gePow10 sig exp (e + 1) then log10Up k sig exp (e + 1) else e

/-- The downward half of the same walk (helper). -/
def log10Down : Nat → Nat → Int → Int → Int
  | 0, _, _, e => e
  | k + 1, sig, exp, e => if gePow10 sig exp e then e else log10Down k sig exp (e - 1)

/-- `⌊log₁₀ (sig · 2^exp)⌋`, for a positive value (helper). -/
def log10Floor (sig : Nat) (exp : Int) : Int :=
  let est : Int := ((bitLen sig : Int) + exp - 1) * 30103 / 100000
  if gePow10 sig exp est then log10Up 8 sig exp est else log10Down 8 sig exp (est - 1)

/-- The shortest-digits search, structural on the remaining digit budget: at
each length the nearest candidate is tried first and its two neighbours after
it, which covers the interval's asymmetry at a power of two (helper). -/
def shortestAux (w : FloatWidth) (sig : Nat) (exp : Int) (e10 : Int) :
    Nat → Nat → List Char × Int
  | 0, L =>
      -- Unreachable: `maxDigits` significant digits always round-trip.
      let n : Int := e10 - (L : Int) + 1
      let d := nearestDigits sig exp n
      (digitsOf d, e10)
  | k + 1, L =>
      let n : Int := e10 - (L : Int) + 1
      let d0 := nearestDigits sig exp n
      match [d0, d0 + 1, d0 - 1].find? (fun d => d != 0 && roundTrips w sig exp d n) with
      | some d =>
          let ds := digitsOf d
          (stripTrailingZeros ds, n + (ds.length : Int) - 1)
      | none => shortestAux w sig exp e10 k (L + 1)

/-- The shortest decimal that round-trips: its significant digits and the
decimal exponent of its leading digit (`3.12:40`). -/
def shortestDigits (w : FloatWidth) (sig : Nat) (exp : Int) : List Char × Int :=
  shortestAux w sig exp (log10Floor sig exp) w.maxDigits 1

/-- `3.12:41`'s layout, given the significant digits and the decimal exponent
of the leading one (helper). -/
def layout (w : FloatWidth) (ds : List Char) (e : Int) : String :=
  let digits := String.ofList ds
  let len : Int := (ds.length : Int)
  if w.fixedLo ≤ e ∧ e ≤ w.fixedHi then
    if len - 1 ≤ e then
      -- `1234e7 → 12340000000.0`
      digits ++ String.ofList (List.replicate (e + 1 - len).toNat '0') ++ ".0"
    else if 0 ≤ e then
      -- `1234e-2 → 12.34`
      String.ofList (ds.take (e + 1).toNat) ++ "." ++ String.ofList (ds.drop (e + 1).toNat)
    else
      -- `1234e-6 → 0.001234`
      "0." ++ String.ofList (List.replicate (-e - 1).toNat '0') ++ digits
  else
    let head := String.ofList (ds.take 1)
    let rest := if ds.length > 1 then "." ++ String.ofList (ds.drop 1) else ""
    let sign := if e < 0 then "-" else "+"
    head ++ rest ++ "e" ++ sign ++ natDecimal e.natAbs

/-- **The text `@dbg` prints for a float** (`3.12:39`–`3.12:42`). A NaN is
`NaN` whatever its sign — which is why `σ_NaN`, a *target* parameter (§2),
never reaches a printed expectation through `@dbg`; only `@total_cmp` can see
it. `±inf` are `inf` and `-inf`, `±0` are `0.0` and `-0.0`, and a finite value
is its shortest round-trip digits laid out by `3.12:41`. -/
def FloatDatum.render (w : FloatWidth) : FloatDatum → String
  | .nan _ => "NaN"
  | .inf true => "-inf"
  | .inf false => "inf"
  | .num neg sig exp =>
      if sig = 0 then (if neg then "-0.0" else "0.0")
      else
        let (ds, e) := shortestDigits w sig exp
        (if neg then "-" else "") ++ layout w ds e

/-! ## The literal form, and how it prints

A core float literal carries a **decimal** — `sig · 10^(±e)` with a natural
significand — rather than a datum: elaboration has already resolved the type
(`4.1:2`'s float counterpart, `3.12:7`), but `3.12:9` makes the *value* the
correctly-rounded reading of that decimal, which is the model's `ofLit`. The
printer reproduces the decimal exactly (`Print.lean`), so the compiler reads
back the digits the core holds and applies its own `3.12:9`; nothing depends
on the two agreeing about a rendering. A literal is non-negative, as Rue's
grammar writes one; a negative value is `neg` applied to it (`3.12:24`). -/

/-- A core float literal: the decimal `sig · 10^e` (`negExp` flips the
exponent's sign), which `3.12:9` reads at the form's width. -/
structure FloatLit where
  /-- The decimal significand, as written. -/
  sig : Nat
  /-- Whether the decimal exponent is negative. -/
  negExp : Bool
  /-- The decimal exponent's magnitude. -/
  e : Nat
deriving DecidableEq, Repr

/-- The Rue source spelling of a literal: positional, and always with a
fractional part so that the token lexes as a float and not as an integer
(`3.12:41` asks the same of a *rendering*; this is the printer's side). The
text denotes the decimal **exactly**, so the compiler's `3.12:9` and the
model's `ofLit` read the same number. -/
def FloatLit.spell (l : FloatLit) : String :=
  let ds := digitsOf l.sig
  if l.negExp && l.e ≠ 0 then
    if l.e < ds.length then
      String.ofList (ds.take (ds.length - l.e)) ++ "." ++ String.ofList (ds.drop (ds.length - l.e))
    else
      "0." ++ String.ofList (List.replicate (l.e - ds.length) '0') ++ String.ofList ds
  else
    String.ofList ds ++ String.ofList (List.replicate l.e '0') ++ ".0"

/-- The literal's exact decimal value as a rational `num / den`, which
`3.12:9`'s rounding is applied to (helper). -/
def FloatLit.exact (l : FloatLit) : Nat × Nat :=
  if l.negExp then (l.sig, 10 ^ l.e) else (l.sig * 10 ^ l.e, 1)

/-! ## `Float.exactOps` — the executable instance

Constructive throughout: every rounded operation is an exact rational (or, for
`@sqrt`, an exact integer square root) handed to `roundRat`. Nothing here uses
Lean's `Float`, so nothing here can put `Classical.choice` on a theorem, and
the corpus runs by ordinary evaluation.

What is **not** proved is that these definitions satisfy `FloatModel`'s laws —
that is the slice's standing assumption (`TRUST.md`), the same one §7 makes
when it discharges the totality lemma "against the standard rather than
against Rue". It is checked instead: every corpus case's printed value is
compared against the compiler. -/

namespace Float

/-- The exact value of a finite datum as a signed rational `(neg, num, den)`
(helper). -/
def ratOf (neg : Bool) (sig : Nat) (exp : Int) : Bool × Nat × Nat :=
  (neg, sig * 2 ^ (max 0 exp).toNat, 2 ^ (max 0 (-exp)).toNat)

/-- Add two signed rationals, reporting the sign of the exact sum and its
magnitude (helper). -/
def ratAdd (n₁ : Bool) (a₁ b₁ : Nat) (n₂ : Bool) (a₂ b₂ : Nat) : Bool × Nat × Nat :=
  let x := a₁ * b₂
  let y := a₂ * b₁
  let d := b₁ * b₂
  if n₁ = n₂ then (n₁, x + y, d)
  else if y ≤ x then (n₁, x - y, d)
  else (n₂, y - x, d)

/-! ### NaN propagation, and what `σ` is for

`σ` is `σ_NaN`, and it is the sign of a NaN these operations **create** — the
invalid operations `inf - inf`, `0 · inf`, `0/0`, `inf/inf` and `@sqrt` of a
negative. A NaN *operand* is **propagated**: the result is that same NaN, sign
and all, and with two NaN operands the **first** one wins. That is what x86-64
and AArch64 both do, and it is measured against the compiler (`x + (-NaN)` and
`(-NaN) + x` both keep the negative sign at every operator), not inferred.

It is a *model* choice all the same. `FloatModel.arith_nan` assumes only that
*a* NaN comes out, because that is all IEEE 754 promises; which NaN is what
this instance picks, and the corpus is what checks the pick. -/

/-- `(D-Float-Arith)` at `+`: IEEE's special cases as §6.4 lists them, then
the exact sum rounded by `rnd_w`. A NaN operand is propagated (the first, when
both are); `inf - inf` is a NaN this operation creates, so it gets `σ`. The
zero cases are IEEE's for round to nearest: two zeros give `-0` only when both
are, and a sum that is exactly zero is `+0` (helper). -/
def addD (σ : Bool) (w : FloatWidth) : FloatDatum → FloatDatum → FloatDatum
  | .nan b, _ => .nan b
  | _, .nan b => .nan b
  | .inf x, .inf y => if x = y then .inf x else .nan σ
  | .inf x, .num _ _ _ => .inf x
  | .num _ _ _, .inf y => .inf y
  | .num n₁ s₁ e₁, .num n₂ s₂ e₂ =>
      if s₁ = 0 && s₂ = 0 then .num (n₁ && n₂) 0 0
      else
        let (r₁, x₁, y₁) := ratOf n₁ s₁ e₁
        let (r₂, x₂, y₂) := ratOf n₂ s₂ e₂
        let (sg, num, den) := ratAdd r₁ x₁ y₁ r₂ x₂ y₂
        if num = 0 then .num false 0 0 else roundRat w sg num den

/-- `(D-Float-Arith)` at `*`: `0 × inf` is a NaN, every other infinite case is
the infinity of the xor sign, and a finite product is exact then rounded. A NaN
operand is propagated; `0 · inf` is a NaN this operation creates, so it gets `σ`
(helper). -/
def mulD (σ : Bool) (w : FloatWidth) : FloatDatum → FloatDatum → FloatDatum
  | .nan b, _ => .nan b
  | _, .nan b => .nan b
  | .inf x, .inf y => .inf (xor x y)
  | .inf x, .num n s _ => if s = 0 then .nan σ else .inf (xor x n)
  | .num n s _, .inf y => if s = 0 then .nan σ else .inf (xor n y)
  | .num n₁ s₁ e₁, .num n₂ s₂ e₂ =>
      let sg := xor n₁ n₂
      if s₁ = 0 || s₂ = 0 then .num sg 0 0
      else
        let (_, x₁, y₁) := ratOf false s₁ e₁
        let (_, x₂, y₂) := ratOf false s₂ e₂
        roundRat w sg (x₁ * x₂) (y₁ * y₂)

/-- `(D-Float-Arith)` at `/`: `3.12:22`'s two clauses — a finite non-zero over
a zero is the infinity of the xor sign, and `0/0` is a NaN — plus the infinite
cases, and the exact quotient rounded otherwise. A NaN operand is propagated;
`0/0` and `inf/inf` are NaNs this operation creates, so they get `σ`
(helper). -/
def divD (σ : Bool) (w : FloatWidth) : FloatDatum → FloatDatum → FloatDatum
  | .nan b, _ => .nan b
  | _, .nan b => .nan b
  | .inf _, .inf _ => .nan σ
  | .inf x, .num n _ _ => .inf (xor x n)
  | .num n _ _, .inf y => .num (xor n y) 0 0
  | .num n₁ s₁ e₁, .num n₂ s₂ e₂ =>
      let sg := xor n₁ n₂
      if s₂ = 0 then (if s₁ = 0 then .nan σ else .inf sg)
      else if s₁ = 0 then .num sg 0 0
      else
        let (_, x₁, y₁) := ratOf false s₁ e₁
        let (_, x₂, y₂) := ratOf false s₂ e₂
        roundRat w sg (x₁ * y₂) (y₁ * x₂)

/-- `(D-Float-Arith)` at `-`: addition of the negation, which `3.12:24` makes a
sign flip and nothing else — **except** on a NaN operand, which is propagated
unchanged rather than negated. `-` is one machine instruction, not a negation
followed by an addition, so `1.0 - (-NaN)` is `-NaN` on both targets, and the
compiler agrees (helper). -/
def subD (σ : Bool) (w : FloatWidth) : FloatDatum → FloatDatum → FloatDatum
  | .nan b, _ => .nan b
  | _, .nan b => .nan b
  | a, b => addD σ w a b.negate

/-- §6.4's `f₁ ⊕_w f₂` over the four operators. -/
def arith (σ : Bool) (w : FloatWidth) : FloatArith → FloatDatum → FloatDatum → FloatDatum
  | .add, a, b => addD σ w a b
  | .sub, a, b => subD σ w a b
  | .mul, a, b => mulD σ w a b
  | .div, a, b => divD σ w a b

/-- `rnd_w` of the exact square root (`3.12:35`), by an integer square root at
enough extra bits that the sticky information the tie case needs survives. A NaN
operand is propagated; `@sqrt` of a negative (`-inf` included) creates a NaN, so
that one gets `σ`. -/
def sqrtD (σ : Bool) (w : FloatWidth) (f : FloatDatum) : FloatDatum :=
  match f with
  | .nan b => .nan b
  | .inf true => .nan σ
  | .inf false => .inf false
  | .num neg sig exp =>
      if sig = 0 then .num neg 0 0
      else if neg then .nan σ
      else
        -- `t ≤ exp / 2` keeps `sig · 2^(exp - 2t)` a natural number, and the
        -- `2p + 4` slack leaves the integer root more than `p` bits.
        let t : Int := Int.fdiv exp 2 - (w.prec : Int) - 2
        let a : Nat := sig * 2 ^ (exp - 2 * t).toNat
        let r : Nat := Nat.sqrt a
        let exactSq : Bool := r * r = a
        let k : Nat := bitLen r - w.prec
        let m0 : Nat := r / 2 ^ k
        let rest : Nat := r % 2 ^ k
        let half : Nat := 2 ^ k / 2
        let up : Bool :=
          if rest > half then true
          else if rest < half then false
          else if !exactSq then true
          else m0 % 2 = 1
        let m : Nat := if up then m0 + 1 else m0
        -- Rounding up from just below `2^p` lands on `2^p`; renormalize once.
        if m = 2 ^ w.prec then canonNum false (2 ^ (w.prec - 1)) (t + (k : Int) + 1)
        else canonNum false m (t + (k : Int))

/-- `3.12:9` on a literal: `rnd_w` of its exact decimal. -/
def ofLit (w : FloatWidth) (sig : Nat) (negExp : Bool) (e : Nat) : FloatDatum :=
  let (num, den) := FloatLit.exact { sig := sig, negExp := negExp, e := e }
  roundRat w false num den

/-- `(D-Int-To-Float)` §6.4: `rnd_w` of an exact integer (`3.12:16`). -/
def ofInt (w : FloatWidth) (n : Int) : FloatDatum :=
  roundRat w (decide (n < 0)) n.natAbs 1

/-- The narrowing half of `(D-Float-Cast)`: `rnd_32` of an `f64` datum
(`3.12:19`), which yields `±inf` when the magnitude is too large for `𝔽_32`
and carries a special across. A converted NaN is a *propagated* NaN, not one
the conversion creates, so it keeps the operand's sign — measured against the
compiler, which casts a negative `f64` NaN to a negative `f32` NaN. `σ_NaN`
therefore never reaches this operation, and it takes no `σ` parameter. -/
def narrow (f : FloatDatum) : FloatDatum :=
  match f with
  | .nan b => .nan b
  | .inf b => .inf b
  | .num neg sig exp =>
      if sig = 0 then .num neg 0 0
      else
        let (_, num, den) := ratOf false sig exp
        roundRat .w32 neg num den

/-- The executable operations the corpus, the printer and the `#eval` demos
run on: `σ_NaN` is **positive**, the AArch64 choice of `3.12:44` and Appendix
B.1, which is the host this slice's corpus was checked against. Positive is
`false` here, because `FloatDatum.nan` carries the sign as `neg` — the field
is the *sign bit*, so `nanSign := false` is `+NaN` and `true` is `-NaN`, and
`FloatDatum.totalRank (.nan false) = 3`, the top of `≺_w`. Flipping this one
`Bool` is the whole of retargeting the instance to x86-64. -/
def exactOps : FloatOps where
  arith := arith false
  sqrt := sqrtD false
  ofLit := ofLit
  ofInt := ofInt
  narrow := narrow
  nanSign := false

end Float

end RueCore
