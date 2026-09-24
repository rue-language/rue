import RueCore.Corpus
import RueCore.Gen

/-!
`lake exe ruecore-corpus` prints the bridge corpus as JSON on stdout. With
`--gen N --seed S` (RUE-2229) the `N` programs `RueCore/Gen.lean` generates
from seed `S` follow the seed cases in the same document; the seed defaults
to 0. Without `--gen` the output is the seed corpus alone, exactly as Buck's
`corpus.json` expects it. A generated case the export fuel does not complete is
an error (exit 1, the cases named on stderr) rather than a silent omission,
because `Gen.lean` guarantees every one terminates.
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
      IO.print (RueCore.Corpus.jsonOf (RueCore.Corpus.cases ++ gen))
      return 0
