import RueCore.Corpus
import RueCore.Gen
import RueCore.Explain.Text

/-!
`lake exe ruecore-corpus` prints the bridge corpus as JSON on stdout. With
`--gen N --seed S` (RUE-2229) the `N` programs `RueCore/Gen.lean` generates
from seed `S` follow the seed cases in the same document; the seed defaults
to 0. With `--profile` (RUE-2469) it prints instead the checker's acceptance
profile over the same cases: how many `checkProgram` accepts and rejects, and
each group's outcomes under `run`, a refusal by its violation. Without `--gen` the output is the seed corpus alone, exactly as Buck's
`corpus.json` expects it. A generated case the export fuel does not complete is
an error (exit 1, the cases named on stderr) rather than a silent omission,
because `Gen.lean` guarantees every one terminates.

The profile also names the checker's **incompleteness** (RUE-2491): a case
`checkProgram` refuses whose `run` nonetheless reaches an `ok` value at the
export fuel and model — the checker rejects it, but the interpreter did not
get stuck on it. Such a case's verdict is never checked against the
compiler's *run*, only against its accept/reject call (`Corpus.lean`'s
module docstring), so an over-strict rejection here is invisible to the
bridge otherwise. Each one is listed with the checker's own refusal, in the
calculus's own words (`Explain.lean`'s `verdictSection`, the same prose
`ruecore-explain` prints), so a reader can tell an intended static
approximation from an accidental one without re-deriving it by hand.
-/

/-- (helper) The command line. -/
structure Options where
  gen : Option Nat := none
  seed : Nat := 0
  profile : Bool := false

/-- (helper) Usage, printed on a bad argument. -/
def usage : String := "usage: ruecore-corpus [--gen N] [--seed S] [--profile]"

/-- (helper) Parse the arguments, rejecting anything unknown. -/
def parseArgs : List String → Options → Except String Options
  | [], o => .ok o
  | "--profile" :: rest, o => parseArgs rest { o with profile := true }
  | "--gen" :: n :: rest, o =>
      match n.toNat? with
      | some k => parseArgs rest { o with gen := some k }
      | none => .error s!"--gen needs a count, got {n}\n{usage}"
  | "--seed" :: s :: rest, o =>
      match s.toNat? with
      | some k => parseArgs rest { o with seed := k }
      | none => .error s!"--seed needs a natural number, got {s}\n{usage}"
  | ["--gen"], _ => .error s!"--gen needs a count\n{usage}"
  | ["--seed"], _ => .error s!"--seed needs a natural number\n{usage}"
  | arg :: _, _ => .error s!"unknown argument {arg}\n{usage}"

/-- (helper) An outcome's name in the profile: `ok`, `panic`, `outOfFuel`, or
`stuck <violation>`. -/
def outcomeKey : RueCore.EvalRes → String
  | .ok .. => "ok"
  | .returned .. => "returned"
  | .broke .. => "broke"
  | .panic .. => "panic"
  | .stuck w => s!"stuck {RueCore.Corpus.violationName w}"
  | .outOfFuel => "outOfFuel"

/-- (helper) Count each key, in first-seen order. -/
def tally (keys : List String) : List (String × Nat) :=
  keys.foldl (init := []) fun acc k =>
    if acc.any (·.1 == k) then acc.map fun (k', n) => if k' == k then (k', n + 1) else (k', n)
    else acc ++ [(k, 1)]

/-- (helper) The checker's refusal for a rejected program, in the calculus's
own words: the failing premise's rule and citation, or (when no function
body's own derivation failed) the whole-program premise (`Explain.lean`'s
`Text.verdictSection`, whose rejection prose this reuses verbatim so the
profile's reason and `ruecore-explain`'s stay one text rather than two). -/
def refusalLines (P : RueCore.Program) : List String :=
  (RueCore.Explain.Text.verdictSection P (RueCore.Explain.programDerivs P 0 P.fns)).drop 2

/-- (helper) One group of the profile: the cases, how many `checkProgram`
accepts and rejects, each side's outcomes at the export fuel and model, and
(RUE-2491) the rejected cases whose `run` nonetheless reaches an `ok` value —
the checker's incompleteness, listed with each one's refusal. -/
def profileLines (title : String) (cs : List RueCore.Corpus.Case) : List String :=
  let judged := cs.map fun c =>
    (RueCore.checkProgram c.prog,
     outcomeKey (RueCore.run RueCore.Corpus.exportOps c.prog RueCore.Corpus.exportFuel), c)
  let acc := judged.filter (·.1)
  let rej := judged.filter (fun j => !j.1)
  let cleanRej := rej.filter (fun j => j.2.1 == "ok")
  let row (xs : List (Bool × String × RueCore.Corpus.Case)) : String :=
    ", ".intercalate ((tally (xs.map (·.2.1))).map fun (k, n) => s!"{k} {n}")
  [s!"{title}: {cs.length} programs; checkProgram accepts {acc.length}, rejects {rej.length}",
   s!"  accepted, by outcome: {row acc}",
   s!"  rejected, by outcome: {row rej}",
   s!"  rejected but runs cleanly (checker refuses, `run` reaches a value; RUE-2491): " ++
     s!"{cleanRej.length}"] ++
  (cleanRej.flatMap fun j => [s!"    {j.2.2.name}:"] ++ (refusalLines j.2.2.prog).map ("      " ++ ·))

/-- (helper) Print the corpus, with the generated cases when asked; exit 2 on
a bad argument. -/
def main (args : List String) : IO UInt32 := do
  match parseArgs args {} with
  | .error msg =>
      IO.eprintln msg
      return 2
  | .ok o =>
      let gen := match o.gen with
        | none => []
        | some n => RueCore.Gen.generate n o.seed
      -- `jsonOf` leaves out a case the export fuel does not complete, which
      -- is right for a hand-written seed but, for a generated one, would hide
      -- a draw that broke the generator's termination guarantee (`Gen.lean`,
      -- "Loops"). So a generated case that does not complete is an error.
      let unfinished := gen.filter (fun c => !RueCore.Corpus.completed c)
      if !unfinished.isEmpty then
        IO.eprintln (s!"ruecore-corpus: {unfinished.length} generated case(s) did not complete " ++
          "at the export fuel, which the generator guarantees they do: " ++
          ", ".intercalate (unfinished.map (·.name)))
        return 1
      if o.profile then
        let genTitle := match o.gen with
          | none => []
          | some n => profileLines s!"generated (--gen {n} --seed {o.seed})" gen
        IO.println ("\n".intercalate
          (["checker acceptance profile (RUE-2469): run at the export fuel and model"] ++
            profileLines "seed corpus" RueCore.Corpus.cases ++ genTitle))
        return 0
      IO.print (RueCore.Corpus.jsonOf (RueCore.Corpus.cases ++ gen))
      return 0
