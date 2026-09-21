import RueCore.Corpus
import RueCore.Gen

/-!
`lake exe ruecore-corpus` prints the bridge corpus as JSON on stdout. With
`--gen N --seed S` (RUE-2229) the `N` programs `RueCore/Gen.lean` generates
from seed `S` follow the seed cases in the same document; the seed defaults
to 0. Without `--gen` the output is the seed corpus alone, exactly as Buck's
`corpus.json` expects it.
-/

/-- (helper) The command line. -/
structure Options where
  gen : Option Nat := none
  seed : Nat := 0

/-- (helper) Usage, printed on a bad argument. -/
def usage : String := "usage: ruecore-corpus [--gen N] [--seed S]"

/-- (helper) Parse the arguments, rejecting anything unknown. -/
def parseArgs : List String → Options → Except String Options
  | [], o => .ok o
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

/-- (helper) Print the corpus, with the generated cases when asked; exit 2 on
a bad argument. -/
def main (args : List String) : IO UInt32 := do
  match parseArgs args {} with
  | .error msg =>
      IO.eprintln msg
      return 2
  | .ok o =>
      let cases := match o.gen with
        | none => RueCore.Corpus.cases
        | some n => RueCore.Corpus.cases ++ RueCore.Gen.generate n o.seed
      IO.print (RueCore.Corpus.jsonOf cases)
      return 0
