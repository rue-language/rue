import Lean

/-!
# RueCore.Layers — the layer table (RUE-2456)

The one place a module's layer is declared (README, "Layers"). Two tools read
it: the layering audit `lake exe ruecore-layers` (`RueCore/LayersMain.lean`),
which checks the import graph against it, and the trusted-base lint `lake exe
ruecore-lint` (`RueCore/Lint.lean`, RUE-2457), which forbids `unsafe`,
`partial`, `@[implemented_by]`, `@[extern]` and `opaque` in the modules it
puts in L0–L2 and the Spec layer. It imports only `Lean`, so both executables build without the
calculus.
-/

open Lean

namespace RueCore.Layers

/-- (helper) The layer table: every module of the package and its layer,
`0`–`4` (`layerName`). The one place a layer is declared. -/
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
  -- Spec: the statements, over L0 and L1 alone
  (`RueCore.Spec.Safety, 2),
  (`RueCore.Spec.Checker, 2),
  (`RueCore.Spec.Trace, 2),
  (`RueCore.Spec.Step, 2),
  (`RueCore.Spec.Adequacy, 2),
  (`RueCore.Spec, 2),
  -- L2 proofs
  (`RueCore.Statics.Lemmas, 3),
  (`RueCore.Dynamics.Lemmas, 3),
  (`RueCore.Step.Lemmas, 3),
  (`RueCore.Soundness, 3),
  (`RueCore.Checker, 3),
  (`RueCore.Trace, 3),
  (`RueCore.Adequacy, 3),
  (`RueCore.TraceExact, 3),
  (`RueCore.TraceOrder, 3),
  (`RueCore.Spine, 3),
  -- L3 tooling
  (`RueCore.Examples, 4),
  (`RueCore.Witnesses, 4),
  (`RueCore.Print, 4),
  (`RueCore.Corpus, 4),
  (`RueCore.Gen, 4),
  (`RueCore.Explain, 4),
  (`RueCore.Explain.Ledger, 4),
  (`RueCore.Explain.Text, 4),
  (`RueCore.Explain.Html, 4),
  (`RueCore.Digest, 4),
  (`RueCore.CorpusMain, 4),
  (`RueCore.ExplainMain, 4),
  (`RueCore.DigestMain, 4),
  (`RueCore.LayersMain, 4),
  (`RueCore.Layers, 4),
  (`RueCore.Lint, 4),
  (`RueCore.LintMain, 4),
  (`RueCore, 4)
]

/-- (helper) The roots the audit walks from: the library root and every
`lean_exe` root in `lakefile.toml`. -/
def roots : List Name :=
  [`RueCore, `RueCore.CorpusMain, `RueCore.ExplainMain, `RueCore.DigestMain, `RueCore.LayersMain,
    `RueCore.LintMain]

/-- (helper) A layer's name, as the README's table writes it. The Spec
layer (RUE-2460) sits between L1 and L2 and keeps its own name, so the
others keep theirs. -/
def layerName : Nat → String
  | 0 => "L0 syntax"
  | 1 => "L1 definitions"
  | 2 => "Spec statements"
  | 3 => "L2 proofs"
  | _ => "L3 tooling"

/-- (helper) The Spec layer's number: the statements, over L0 and L1 alone. -/
def specLayer : Nat := 2

/-- (helper) The tooling layer's number, L3. Every layer below it — L0, L1,
Spec and L2, the layers a claim may rest on — is held to the module system,
imports only `Init` from outside the package, and is linted for the
constructs `Lint.lean` forbids. -/
def toolingLayer : Nat := 4

/-- (helper) Is this module one of the package's? -/
def inPackage (m : Name) : Bool := (`RueCore).isPrefixOf m

/-- (helper) The layer `table` declares for a module, if any. -/
def layerOf? (m : Name) : Option Nat := (table.find? (·.1 == m)).map (·.2)

end RueCore.Layers
