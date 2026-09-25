import Lean

/-!
# RueCore.Layers — the layer table (RUE-2456)

The one place a module's layer is declared (README, "Layers"). Two tools read
it: the layering audit `lake exe ruecore-layers` (`RueCore/LayersMain.lean`),
which checks the import graph against it, and the trusted-base lint `lake exe
ruecore-lint` (`RueCore/Lint.lean`, RUE-2457), which forbids `unsafe`,
`partial`, `@[implemented_by]`, `@[extern]` and `opaque` in the modules it
puts in L0–L2. It imports only `Lean`, so both executables build without the
calculus.
-/

open Lean

namespace RueCore.Layers

/-- (helper) The layer table: every module of the package and its layer,
`0`–`3`. The one place a layer is declared. -/
def table : List (Name × Nat) := [
  -- L0 syntax
  (`RueCore.Float, 0),
  (`RueCore.Syntax, 0),
  -- L1 definitions: the semantics, and what the headline statements are written in
  (`RueCore.Statics, 1),
  (`RueCore.Dynamics, 1),
  (`RueCore.Step, 1),
  (`RueCore.Checker.Defs, 1),
  (`RueCore.Soundness.Defs, 1),
  (`RueCore.Trace.Defs, 1),
  (`RueCore.Adequacy.Defs, 1),
  -- L2 proofs
  (`RueCore.Soundness, 2),
  (`RueCore.Checker, 2),
  (`RueCore.Trace, 2),
  (`RueCore.Adequacy, 2),
  (`RueCore.TraceExact, 2),
  (`RueCore.TraceOrder, 2),
  -- L3 tooling
  (`RueCore.Examples, 3),
  (`RueCore.Witnesses, 3),
  (`RueCore.Print, 3),
  (`RueCore.Corpus, 3),
  (`RueCore.Gen, 3),
  (`RueCore.Explain, 3),
  (`RueCore.Explain.Ledger, 3),
  (`RueCore.Explain.Text, 3),
  (`RueCore.Explain.Html, 3),
  (`RueCore.Digest, 3),
  (`RueCore.CorpusMain, 3),
  (`RueCore.ExplainMain, 3),
  (`RueCore.DigestMain, 3),
  (`RueCore.LayersMain, 3),
  (`RueCore.Layers, 3),
  (`RueCore.Lint, 3),
  (`RueCore.LintMain, 3),
  (`RueCore, 3)
]

/-- (helper) The roots the audit walks from: the library root and every
`lean_exe` root in `lakefile.toml`. -/
def roots : List Name :=
  [`RueCore, `RueCore.CorpusMain, `RueCore.ExplainMain, `RueCore.DigestMain, `RueCore.LayersMain,
    `RueCore.LintMain]

/-- (helper) A layer's name, as the README's table writes it. -/
def layerName : Nat → String
  | 0 => "L0 syntax"
  | 1 => "L1 definitions"
  | 2 => "L2 proofs"
  | _ => "L3 tooling"

/-- (helper) Is this module one of the package's? -/
def inPackage (m : Name) : Bool := (`RueCore).isPrefixOf m

/-- (helper) The layer `table` declares for a module, if any. -/
def layerOf? (m : Name) : Option Nat := (table.find? (·.1 == m)).map (·.2)

end RueCore.Layers
