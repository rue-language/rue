import RueCore.Corpus
import RueCore.Gen

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

/-- (helper) One group of the profile: the cases, how many `checkProgram`
accepts and rejects, and each side's outcomes at the export fuel and model. -/
def profileLines (title : String) (cs : List RueCore.Corpus.Case) : List String :=
  let judged := cs.map fun c =>
    (RueCore.checkProgram c.prog,
     outcomeKey (RueCore.run RueCore.Corpus.exportOps c.prog RueCore.Corpus.exportFuel))
  let acc := judged.filter (·.1)
  let rej := judged.filter (!·.1)
  let row (xs : List (Bool × String)) : String :=
    ", ".intercalate ((tally (xs.map (·.2))).map fun (k, n) => s!"{k} {n}")
  [s!"{title}: {cs.length} programs; checkProgram accepts {acc.length}, rejects {rej.length}",
   s!"  accepted, by outcome: {row acc}",
   s!"  rejected, by outcome: {row rej}"]

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
