import RueCore.Lint

/-!
# `lake exe ruecore-lint` — the trusted-base lint (RUE-2457)

```
ruecore-lint     lint every declaration of the package; exit 1 on a violation
```

What it checks is `RueCore/Lint.lean`'s module docstring: every package
declaration's axioms against the allow-list `propext`, `Quot.sound`; no
`unsafe`, `partial`, `@[implemented_by]`, `@[extern]`, `opaque` or
compiler-evaluation primitive in L0–L2 or Spec, with L3's uses listed; no
`debug.skipKernelTC`, no unbounded `maxHeartbeats` and no macro-named option
in the sources, with every bounded override listed; and the size of the
headline statements' trusted base, which `TRUST.md` prints in full. The
options check is a scan of the sources and a courtesy: the guarantee that
the kernel checked every declaration is `leanchecker`'s re-check of every
module in the roots' import closure outside the toolchain (`lake env
leanchecker $(lake exe ruecore-layers --closure)`), which `bin/chain.sh` and
the Buck target run beside this lint.

"Every declaration of the package" is asked of the environment, not of the
sources: the lint imports each root of `RueCore/Layers.lean` in turn — the
library root, then each executable's root, since each of those defines its
own `main` and no one environment can hold them all — and lints every
constant declared in a package module, generated ones included, once. It
fails if a module of the layer table is imported by no root.

Run it from the package directory after `lake build` and building the
executables (their `.olean`s are what it imports). Importing an environment
at run time needs `enableInitializersExecution`, so the entry point is the
usual `unsafe`/`implemented_by` pair, which the lint itself lists under L3.
-/

open Lean RueCore

/-- (helper) Import one root's environment. Initializer execution is enabled
before each import, because an import resets it. -/
unsafe def importRoot (root : Name) : IO Environment := do
  enableInitializersExecution
  importModules #[{ module := root }] {} (trustLevel := 1024) (loadExts := true)

/-- (helper) Print the findings as a table. -/
def printTable (title : String) (rows : Array Lint.Finding) : IO Unit := do
  IO.println s!"## {title}"
  IO.println ""
  if rows.isEmpty then
    IO.println "none"
  else
    IO.println "| Layer | Where | Found | Verdict |"
    IO.println "| --- | --- | --- | --- |"
    for f in rows do
      IO.println s!"| {Lint.layerLabel f.layer} | `{f.subject}` | {f.detail} | {if f.fails then "**fails**" else "listed"} |"
  IO.println ""

/-- (helper) Run the lint and return the exit code. -/
unsafe def lintMain : IO UInt32 := do
  initSearchPath (← findSysroot)
  let mut findings : Array Lint.Finding := #[]
  let mut linted : NameSet := {}
  let mut modules : NameSet := {}
  let mut memo : NameMap (Array Name) := {}
  let mut used : Array Name := #[]
  let mut problems : Array String := #[]
  let mut base? : Option Lint.TrustedBase := none
  let mut layerEnv? : Option Environment := none
  let mut spine : Array String := #["the library root was not linted, so the spine was not checked"]
  let mut sharp : Array String := #[]
  for root in Layers.roots do
    let path ← findOLean root
    if !(← path.pathExists) then
      problems := problems.push s!"{root}: no compiled .olean at {path}; run `lake build` and build the executables first"
      continue
    let env ← importRoot root
    let (fs, l, ms, m, u) := Lint.lintEnvironment env linted memo
    findings := findings ++ fs
    linted := l
    modules := ms.foldl (init := modules) fun acc x => acc.insert x
    memo := m
    used := Lint.union used u
    if root == `RueCore then
      spine := Lint.spineProblems env
      layerEnv? := some env
      let ctx : Core.Context :=
        { fileName := "<ruecore-lint>", fileMap := default, currNamespace := `RueCore }
      let (tb, _) ← (Lint.trustedBase env).toIO ctx { env := env }
      base? := some tb
      let (sp, _) ← (Meta.MetaM.run' (Lint.sharpProblems env)).toIO ctx { env := env }
      sharp := sp
  for (m, _) in Layers.table do
    if !modules.contains m && !problems.any (·.startsWith s!"{m}:") then
      problems := problems.push s!"{m}: in the layer table, but no root imports it"
  let (options, files) ← Lint.optionFindings
  let axiomRows := findings.filter (·.isAxiom)
  let constructRows := findings.filter (!·.isAxiom)
  let usedSorted := used.qsort (·.toString < ·.toString)
  IO.println "# ruecore-lint"
  IO.println ""
  IO.println "The guarantee that the kernel checked every declaration is not this lint: it is `leanchecker`, the toolchain's re-check of every module in the roots' import closure outside the toolchain (`lake env leanchecker $(lake exe ruecore-layers --closure)`), run by `bin/chain.sh` and by the Buck target `root//:lean-ruecore` beside this lint. The options table below comes from a best-effort scan of the sources, a courtesy that names a likely culprit early; a macro or an elaborator can set an option no scan sees, and the scan does not parse every lexical form."
  IO.println ""
  IO.println s!"- Declarations linted: {linted.size}, in {modules.size} modules, from the roots {Layers.roots}."
  IO.println s!"- Allowed axioms: {Lint.allowedAxioms}. Axioms the package's declarations use, at most (an inductive block counts as one node): {usedSorted.toList}."
  IO.println s!"- Distinct constants the axiom pass walked: {memo.size}."
  IO.println s!"- Source files scanned for options: {files}, and lakefile.toml."
  if let some tb := base? then
    IO.println s!"- Trusted base of the {Lint.headline.length} headline statements: {tb.definitions.size} definitions, {tb.instances.size} instances, {tb.generated} Lean-generated auxiliaries (listed in TRUST.md)."
    for h in tb.missing do
      problems := problems.push s!"{h}: a headline theorem or Spec statement the environment does not have"
  if spine.isEmpty then
    IO.println s!"- Spine: {Lint.headline.length} Spec statements (`RueCore.Spec.spine`) and {RueCore.Spec.witnesses.length} non-vacuity witnesses (`RueCore.Spec.witnesses`), naming every spine theorem; each theorem states its `_stmt`'s body word for word (up to binder names), and `RueCore.Spine` restates each as exactly its `_stmt`, checked by the kernel."
  for p in spine do problems := problems.push s!"spine: {p}"
  if sharp.isEmpty && spine.isEmpty then
    IO.println s!"- Sharpness: {RueCore.Spec.sharpness.length} counter-example statements (`RueCore.Spec.sharpness`) and {RueCore.Spec.sharpnessReasons.length} recorded reasons (`RueCore.Spec.sharpnessReasons`) cover every hypothesis of every spine statement (`Lint.hypotheses`), each counter-example its theorem's statement and bound in `RueCore.Spine`."
  for p in sharp do problems := problems.push s!"sharpness: {p}"
  if let some env := layerEnv? then
    let shape := Lint.layerShapeProblems env
    if shape.isEmpty then
      IO.println "- Layer shapes: L1 declares no authored theorem; the Spec layer declares only its lists (`Spec.spine`, `Spec.witnesses`, `Spec.sharpness`, `Spec.sharpnessReasons`) and the `_stmt`s they list."
    for p in shape do problems := problems.push s!"layers: {p}"
  IO.println ""
  printTable "Axioms outside the allow-list (fail, except `Classical.choice` in an L3 definition, listed)" axiomRows
  printTable "Constructs (fail in L0–L2 and Spec, listed in L3)" constructRows
  printTable "Options and kernel evaluation (unbounded or kernel-skipping options fail; bounded ones, and each `decide +kernel`, are listed)" options
  let failing := (findings ++ options).filter (·.fails)
  for f in failing do
    IO.eprintln s!"ruecore-lint: {Lint.layerLabel f.layer} {f.subject}: {f.detail}"
  for p in problems do IO.eprintln s!"ruecore-lint: {p}"
  if failing.isEmpty && problems.isEmpty then
    IO.println s!"ruecore-lint: {linted.size} declarations; {Lint.headline.length} spine statements and {RueCore.Spec.witnesses.length} witnesses, each its theorem's statement and bound in RueCore.Spine, every spine theorem witnessed; {RueCore.Spec.sharpness.length} counter-examples and {RueCore.Spec.sharpnessReasons.length} reasons, every spine hypothesis covered; no authored theorem in L1 and nothing but statements in Spec; axioms within {Lint.allowedAxioms} but for {axiomRows.size} L3 definitions' Classical.choice, listed; no forbidden construct in L0–L2 or Spec, {constructRows.size} uses listed; source scan found no kernel-skipping, unbounded or macro-named option, {options.size} bounded settings and `decide +kernel` uses listed; kernel re-check (leanchecker over the import closure) is the guarantee"
    return 0
  IO.eprintln s!"ruecore-lint: {failing.size + problems.size} violation(s)"
  return 1

/-- (helper) The entry point's safe face. -/
@[implemented_by lintMain] opaque lintImpl : IO UInt32

/-- (helper) `lake exe ruecore-lint`: the trusted-base lint. -/
def main : IO UInt32 := lintImpl
