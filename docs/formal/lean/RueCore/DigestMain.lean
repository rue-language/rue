import RueCore.Lint
import RueCore.Map

/-!
# `lake exe ruecore-digest` — the expert validation surface (RUE-2247)

```
ruecore-digest                 the statement digest, on stdout (DIGEST.md)
ruecore-digest --index <path>  the same, reading the scope counts elsewhere
ruecore-digest --trust         the trust report, on stdout (TRUST.md)
ruecore-digest --spine         the spine: every Spec statement (SPINE.md)
ruecore-digest --challenge     Lean Comparator's challenge (comparator/Challenge.lean)
ruecore-digest --comparator-config   its configuration (comparator/config.json)
ruecore-digest --fingerprint   a hash of each Spec statement (spine-fingerprints.txt)
ruecore-digest --map           the proof map: generated Mermaid diagrams (MAP.md, RUE-2468)
```

The last four are generated from the Spec layer's lists, `RueCore.Spec.spine`,
`RueCore.Spec.witnesses` and `RueCore.Spec.sharpness` (RUE-2460, RUE-2469,
RUE-2485), and exit non-zero when the lint's spine check
(`RueCore.Lint.spineProblems`) finds the list and the environment disagree.

The trust report ends with the headline statements' trusted base, computed by
`RueCore.Lint.trustedBase`, the pass `lake exe ruecore-lint` runs (RUE-2457).

Both reports are generated from the compiled `RueCore` environment, so
regenerating them and diffing against the committed `DIGEST.md` / `TRUST.md`
is how a reviewer checks that neither has drifted (RUE-2241 defers a CI gate
for that diff; nothing in CI runs the Lean build yet).

`--trust` exits non-zero when a proof depends on an axiom outside this
project's policy, after writing the report, so a build step that redirects it
to a file both keeps the evidence and fails. The digest exits non-zero the
same way when one of the two properties it states about itself — closure, and
completeness against `INDEX.md` — does not hold, naming the miss on stderr.

Importing an environment at run time needs `enableInitializersExecution`, so
the entry point is the usual `unsafe`/`implemented_by` pair; nothing about
the mechanization itself is unsafe.
-/

open Lean RueCore

/-- (helper) How to call the executable. -/
def usage : String :=
  "usage: ruecore-digest [--index <path>] | --trust | --spine | --challenge | \
    --comparator-config | --fingerprint | --map"

/-- (helper) The statement digest: theorems, helper lemmas, and the
definitions their statements are written in terms of. -/
def digestReport (index : String) (env : Environment) : CoreM (String × UInt32) := do
  let authored ← Digest.authoredNames env
  let decls := Digest.declarations env authored
  let mut theorems := #[]
  let mut helpers := #[]
  for (name, info) in decls do
    if info matches .thmInfo _ then
      let item ← Digest.readItem env authored name info
      if item.helper then helpers := helpers.push item else theorems := theorems.push item
  let seeds := (theorems ++ helpers).flatMap (·.deps)
  let reached ← Digest.itemClosure env authored seeds.toList #[]
  let definitions := reached.filter fun it => it.kind != "theorem"
  let rendered := theorems ++ helpers ++ definitions
  let entries := rendered.foldl (init := NameSet.empty) fun acc it => acc.insert it.name
  -- The two properties the file's own preamble states, checked before it is
  -- printed: a report that cannot keep them should say so rather than assert
  -- them (RUE-2247's review).
  -- RUE-2479: the predicates a spine hypothesis tests must be printed with a
  -- body, not by signature alone.
  let bodyless := Digest.bodyRequired.filterMap fun n =>
    match rendered.find? (·.name == n) with
    | none => some s!"{n}: required to print its body, but not in the digest"
    | some it =>
        let hasBody := !it.equations.isEmpty || (it.signature.splitOn " :=\n").length > 1
        if hasBody then none else some s!"{n}: printed by signature alone (RUE-2479)"
  let problems := Digest.closureViolations env entries rendered
    ++ Digest.indexCrossCheck env authored entries index ++ bodyless.toArray
  for problem in problems do
    IO.eprintln s!"ruecore-digest: {problem}"
  match Digest.renderDigest index
      (theorems.qsort Digest.bySource) (helpers.qsort Digest.bySource)
      (Digest.topological definitions) with
  | .ok text => return (text, if problems.isEmpty then 0 else 1)
  | .error why =>
      IO.eprintln s!"ruecore-digest: {why}"
      return ("", 1)

/-- (helper) The trust report: every theorem's axioms, the `sorry` count read
from them, and the package's own declared assumptions. -/
def trustReport (env : Environment) : CoreM (String × UInt32) := do
  let authored ← Digest.authoredNames env
  let decls := Digest.declarations env authored
  let mut theorems := #[]
  let mut declared := #[]
  for (name, info) in decls do
    if info matches .thmInfo _ then
      let item ← Digest.readItem env authored name info
      let axioms ← collectAxioms name
      theorems := theorems.push (item, axioms.qsort (fun a b => a.toString < b.toString))
    else if info matches .axiomInfo _ then
      declared := declared.push (← Digest.readItem env authored name info)
  let ordered := theorems.qsort (fun a b => Digest.bySource a.1 b.1)
  let (text, clean) := Digest.renderTrust ordered (declared.qsort Digest.bySource)
  if !clean then
    IO.eprintln "ruecore-digest --trust: a proof depends on an axiom outside the policy"
  let base ← Lint.trustedBase env
  for h in base.missing do
    IO.eprintln s!"ruecore-digest --trust: headline statement {h} is not a theorem of the environment"
  let text := text ++ "\n" ++ String.intercalate "\n" (Lint.renderTrustedBase base)
  return (text, if clean && base.missing.isEmpty then 0 else 1)

/-! ## The spine (RUE-2460) -/

/-- (helper) The title a Spec module's docstring gives itself, after the
em dash: `RueCore.Spec.Safety — type safety over the interpreter (Spec
layer)` gives `type safety over the interpreter`. -/
def moduleTitle (env : Environment) (m : Name) : String :=
  let doc := (getModuleDoc? env m).bind (·[0]?) |>.map (·.doc) |>.getD ""
  let line := ((doc.splitOn "\n").find? (·.startsWith "# ")).getD s!"# {m}"
  let afterDash := (line.splitOn " — ").getD 1 line
  let title := (afterDash.splitOn " (Spec layer)").headD afterDash
  match title.toList with
  | c :: cs => String.ofList (c.toUpper :: cs)
  | [] => title

/-- (helper) The package definitions a statement names directly, a
constructor standing for its type, less instances and Lean's own
auxiliaries: the first things a reader looks up. -/
def directDefs (env : Environment) (body : Lean.Expr) : Array Name := Id.run do
  let mut out : Array Name := #[]
  for c in body.getUsedConstants do
    let c := match env.find? c with
      | some (.ctorInfo v) => v.induct
      | _ => c
    if !Lint.inPackage env c || out.contains c then continue
    if Meta.isInstanceCore env c || Lint.isLeanAux env c then continue
    out := out.push c
  return out

/-- (helper) The witnesses a spine theorem is named by, as `SPINE.md`'s
"Non-vacuous" line writes them (RUE-2469). -/
def witnessesOf (h : Name) : String :=
  let ws := Spec.witnesses.filter fun (_, _, ts) => ts.contains h
  if ws.isEmpty then "no witness (the lint fails on this)"
  else "witnesses " ++ ", ".intercalate (ws.map fun (w, _, _) => s!"`{Digest.shortName w}`")

/-- (helper) Whether a statement has a hypothesis: a binder of a `Prop` type
anywhere in its `∀`/`∧`/`↔`/`∃` structure (`Lint.hypotheses`, the list the
sharpness check numbers). A statement with none (`freed_once`, `step_iff`)
has nothing to satisfy, so its witnesses only apply it at a non-trivial
program, and `SPINE.md` says so. -/
def hasHypotheses (e : Lean.Expr) : MetaM Bool := do
  return !(← Lint.hypotheses e).isEmpty

/-- (helper) A spine theorem's "Sharp" line (RUE-2485): each hypothesis, by
number and text, with the counter-examples of `Spec.sharpness` that drop it,
or its reason from `Spec.sharpnessReasons`. -/
def sharpOf (h : Name) (hyps : Array String) : String :=
  if hyps.isEmpty then "Sharp: no hypotheses to drop."
  else
    let items := (hyps.toList.zipIdx 1).map fun (txt, i) =>
      let exs : List (Name × Name × List (Name × Nat)) :=
        Spec.sharpness.filter fun (_, _, ps) => ps.contains (h, i)
      let why : Option (Name × Nat × String) :=
        Spec.sharpnessReasons.find? fun (t, j, _) => t == h && j == i
      let tail := match exs, why with
        | [], some (_, _, r) => s!"no counter-example: {r}"
        | [], none => "none (the lint fails on this)"
        | _, _ => (if exs.length > 1 then "counter-examples " else "counter-example ") ++ ", ".intercalate (exs.map fun (x : Name × Name × List (Name × Nat)) => s!"`{Digest.shortName x.1}`")
      s!"{i}. `{oneLine txt}` — {tail}"
    "Sharp:\n\n" ++ "\n".intercalate items
where
  oneLine (t : String) : String := " ".intercalate ((t.splitOn "\n").map (·.trimAscii.toString))

/-- (helper) `SPINE.md`: every Spec statement — the Lean statement, its
English reading and calculus paragraph (its doc-comment), the theorem that
proves it, and the definitions it rests on — generated from
`RueCore.Spec.spine`, so it cannot drift from what the kernel checks. -/
def spineReport (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --spine: {p}"
  let base ← Lint.trustedBase env
  let floatStmts := Spec.spine.filter fun (_, s) =>
    match Lint.find? env s with
    | some (.defnInfo v) => v.value.getUsedConstants.contains ``FloatModel
    | _ => false
  -- the sharpness counts (RUE-2495), computed from `Lint.hypothesisList`, the
  -- walk the lint numbers the hypotheses by: every hypothesis of every spine
  -- statement, those a counter-example covers, and those of them inside a
  -- conclusion
  let mut hypTotal := 0
  let mut hypCovered := 0
  let mut hypInConcl := 0
  for (t, s) in Spec.spine do
    let some (.defnInfo v) := Lint.find? env s | continue
    let hs ← Meta.MetaM.run' (Lint.hypothesisList v.value)
    hypTotal := hypTotal + hs.size
    for (hyp, i) in hs.toList.zipIdx 1 do
      if Spec.sharpness.any (fun (_, _, ps) => ps.contains (t, i)) then
        hypCovered := hypCovered + 1
        if hyp.inConclusion then hypInConcl := hypInConcl + 1
  let reasoned := Spec.sharpnessReasons.length
  let pairs := (Spec.sharpness.map (·.2.2.length)).sum
  let mut out : Array String := #[
    "# The spine: what the mechanization claims",
    "",
    "Generated by `lake exe ruecore-digest --spine` from the Spec layer",
    "(`RueCore/Spec.lean`, RUE-2460); do not edit, regenerate it whenever a Spec",
    "statement or its doc-comment changes.",
    "",
    "**Scope.** These statements are about a *fragment* of the core calculus",
    "(`../01-core-calculus.md`; `INDEX.md` draws the boundary rule by rule), and",
    "nothing outside it is proved by omission. The fragment has no loans or borrows",
    "(Λ is empty, so §5.4 is not modelled) and no allocation store (no buffers, views",
    "or containers, §6.13); both are Phase D (RUE-2238, RUE-2240). So these parts of",
    "§7 have **no statement here** (`../03-metatheory.md`):",
    "",
    "- *No use-after-free*, the §6.13 buffer bullet (RUE-2240);",
    "- *Exclusivity / no aliased mutation* (RUE-2238);",
    "- the lemmas §7 names explicitly: loan/drop non-interference, loan-extent",
    "  nesting, root separation, view-intact (RUE-2238) and handle-uniqueness",
    "  preservation (RUE-2240). Float totality is not a theorem either: the rounded",
    "  operations' closure is assumed, as the laws of `FloatModel`, of every model",
    "  the statements quantify over (it is proved of `Float.exactOps`).",
    "",
    s!"{floatStmts.length} of the {Spec.spine.length} statements quantify over `M : FloatModel`, the IEEE 754 laws assumed.",
    "The laws have a model: `Float.exactModel` (`RueCore/Float/Lemmas.lean`) proves every one",
    "of them of the executable instance `Float.exactOps`, so they are jointly satisfiable and",
    s!"those {floatStmts.length} are not vacuous in `M` (`Nonvacuous.exact_model`, RUE-2469). Several statements say",
    "\"`run` is never `.stuck` with violation *v*\": they mean what `eval`'s monitors",
    "watch, since *v* is the tag a monitor raises (`no_violation`, `no_use_after_move`,",
    "`no_use_after_drop` and `no_linear_discard` say so; RUE-2469).",
    "What the proof means for the compiler, in plain language, is RUE-2462's",
    "one-page account, WHAT-IT-MEANS, beside `../README.md` once it lands.",
    "",
    "**The statements.** Each entry is a `def …_stmt : Prop` of the Spec layer,",
    "written over layers L0 and L1 alone, with its English reading and the",
    "`../01-core-calculus.md` §7 paragraph it realizes (and where it is narrower).",
    "`RueCore/Spine.lean` restates each theorem `RueCore.X` as",
    "`RueCore.Spine.X : RueCore.Spec.X_stmt := @RueCore.X`, so the kernel checks every",
    "proof against its statement; `ruecore-lint` checks that the proof layer states",
    "each one word for word. Lean Comparator (`comparator/`, README \"The statement",
    "layer\") replays the `Spine` proofs in its own kernel, with `propext` and",
    "`Quot.sound` only, against a challenge that writes every statement out in full;",
    "`spine-fingerprints.txt` records a hash of each, so a statement that changes",
    "fails the chain until it is regenerated, and the change is reviewed.",
    "",
    "**Non-vacuity.** Under each statement, \"Non-vacuous\" names the witnesses",
    "(`RueCore.Spec.witnesses`, RUE-2469; the last section) that show its hypotheses",
    "hold together of a non-trivial program: accepted by the checker, typed, run to a",
    "value, a panic or divergence and reached by `Step`, with the drops, destructors",
    "or value the witness states. Each pair is checked by the kernel:",
    "`RueCore/Nonvacuous/Glue.lean` applies the statement to the witness's facts, and",
    "the lint requires that application for every pair listed. A statement with no",
    "hypotheses is only applied at a witness's program, and its line says so. The witnesses are Spec statements too, proved in",
    "`RueCore/Nonvacuous.lean` and covered by the kernel, the lint, Comparator and",
    "the fingerprints.",
    "",
    "**Sharpness.** Under each statement, \"Sharp\" lists its hypotheses, numbered as",
    "`Lint.hypotheses` numbers them (its premises of `Prop` type, those inside the",
    "conclusion included), each with the counter-examples that drop it",
    "(`RueCore.Spec.sharpness`, RUE-2485; the last section): a program, written out in",
    "the statement, of which that hypothesis fails, every other holds, and the",
    "conclusion fails. So the hypothesis is needed. A hypothesis with no",
    "counter-example carries a reason (`RueCore.Spec.sharpnessReasons`), and the lint",
    s!"fails on one with neither: {hypCovered} of the {hypTotal} have a counter-example and {reasoned}",
    s!"{if reasoned == 1 then "has" else "have"} a reason. Of the {hypCovered}, {hypInConcl} are premises inside a conclusion, under",
    "an `∧`, an `↔` or an `∃` of it (a `run … = .ok`, a `Steps …` or an `n < fuel` that",
    "a conjunct starts from), not hypotheses about the program. For `drop_order` 2–3,",
    "`eval_sound` 2–3, `run_sim` 1–2, `eval_complete` 2 and 4 and `run_complete` 1 and 3,",
    "the dropped premise is the only thing tying its bound value or trace to the program,",
    "so the counter-example shows only that the conclusion is not a tautology. The walk does not go",
    "under `∨` or `¬`, nor into a definition that is not reducible (`Config.SafeAt`,",
    "`Exact`, `Blocks`, `Lifo`). Each pairing of a counter-example with a (theorem,",
    s!"number) is checked by the kernel ({pairs} pairs): `RueCore/Sharp/Glue.lean` proves,",
    "from the counter-example, the negation of the spine statement with that",
    "hypothesis removed, and the lint computes that weakened statement itself from",
    "the Spec statement and the number (`Lint.dropHyp`, by the walk that numbers the",
    "hypotheses) and requires the glue theorem to state exactly its negation (RUE-2495).",
    "One limit: each counter-example's negated hypothesis is proved through the spine",
    "theorem itself (from the other hypotheses and the failed conclusion, which are",
    "established without it), except where its doc-comment says it is shown directly.",
    "The counter-examples are Spec statements too, proved in",
    "`RueCore/Sharp.lean` and covered by the kernel, the lint, Comparator and the",
    "fingerprints. Several are refusals of `eval`'s monitors, so a machine without a",
    "monitor falsifies one (R3 of `REDTEAM-LOG.md`). The `FloatModel` laws are not",
    "numbered: they are assumptions about the model a statement is instantiated at, not",
    "hypotheses about a program, and every counter-example runs on `Float.exactOps`, a",
    "model of them; that one reason is recorded with `RueCore.Spec.sharpnessReasons`.",
    "",
    s!"A statement means its text plus the {base.definitions.size} definitions the {Lint.headline.length} statements unfold",
    "to (`TRUST.md`, \"Trusted base\"; bodies in `DIGEST.md`); \"Names\" lists",
    "those an entry mentions.",
    ""]
  let mut group : Option Name := none
  for (h, s) in Spec.spine do
    let some (.defnInfo v) := Lint.find? env s | continue
    let m := (Lint.moduleOf? env s).getD .anonymous
    if group != some m then
      group := some m
      out := out ++ #[s!"## {moduleTitle env m}", "", s!"`{m}`", ""]
    let doc := ((← findDocString? env s).map Digest.trimmed).getD "*(no doc-comment)*"
    let stmt ← Meta.MetaM.run' do
      return (← Meta.ppExpr v.value).pretty 90
    let thmModule := (Lint.moduleOf? env h).map toString |>.getD "?"
    let direct := directDefs env v.value
    let closure := Lint.readable env (Lint.unfoldClosure env v.value.getUsedConstants)
    out := out ++ #[s!"### `{Digest.shortName h}`", "",
      doc, "", "```lean", s!"def {Digest.shortName s} : Prop :=", s!"  {("\n  ".intercalate (stmt.splitOn "\n"))}", "```", "",
      s!"Proved by `{Digest.shortName h}` (`{thmModule}`). Names " ++
        ", ".intercalate (direct.toList.map (s!"`{Digest.shortName ·}`")) ++
        s!"; rests on {closure.size} definitions.", "",
      (if ← Meta.MetaM.run' (hasHypotheses v.value) then "Non-vacuous: " ++ witnessesOf h ++ "."
       else "Non-vacuous: no hypotheses to satisfy; applied at a non-trivial program by " ++
         witnessesOf h ++ "."), "",
      sharpOf h (← Meta.MetaM.run' (Lint.hypotheses v.value)), ""]
  out := out ++ #["## Non-vacuity witnesses", "", "`RueCore.Spec.Nonvacuous`", "",
    "Each statement shows the hypotheses of the spine statements it names satisfiable",
    "together, by a program written out in the statement (RUE-2469).", ""]
  for (h, s, ts) in Spec.witnesses do
    let some (.defnInfo v) := Lint.find? env s | continue
    let doc := ((← findDocString? env s).map Digest.trimmed).getD "*(no doc-comment)*"
    let stmt ← Meta.MetaM.run' do
      return (← Meta.ppExpr v.value).pretty 90
    let thmModule := (Lint.moduleOf? env h).map toString |>.getD "?"
    out := out ++ #[s!"### `{Digest.shortName h}`", "",
      doc, "", "```lean", s!"def {Digest.shortName s} : Prop :=", s!"  {("\n  ".intercalate (stmt.splitOn "\n"))}", "```", "",
      s!"Proved by `{Digest.shortName h}` (`{thmModule}`). Witnesses " ++
        ", ".intercalate (ts.map (s!"`{Digest.shortName ·}`")) ++ ".", ""]
  out := out ++ #["## Sharpness counter-examples", "", "`RueCore.Spec.Sharp`", "",
    "Each statement drops the spine hypotheses it names: that hypothesis fails of a",
    "program written out in the statement, every other holds, and the conclusion fails",
    "(RUE-2485). A hypothesis is a spine theorem and its number in `Lint.hypotheses`.", ""]
  for (h, s, ps) in Spec.sharpness do
    let some (.defnInfo v) := Lint.find? env s | continue
    let doc := ((← findDocString? env s).map Digest.trimmed).getD "*(no doc-comment)*"
    let stmt ← Meta.MetaM.run' do
      return (← Meta.ppExpr v.value).pretty 90
    let thmModule := (Lint.moduleOf? env h).map toString |>.getD "?"
    out := out ++ #[s!"### `{Digest.shortName h}`", "",
      doc, "", "```lean", s!"def {Digest.shortName s} : Prop :=", s!"  {("\n  ".intercalate (stmt.splitOn "\n"))}", "```", "",
      s!"Proved by `{Digest.shortName h}` (`{thmModule}`). Drops " ++
        ", ".intercalate (ps.map fun (t, i) => s!"`{Digest.shortName t}` {i}") ++ ".", ""]
  return ("\n".intercalate out.toList, if problems.isEmpty then 0 else 1)

/-- (helper) A Spec statement's body as Lean source that elaborates back to
the same term: printed with every function binder's type, in the namespace
the statement is declared in (`RueCore.Spec`), so that a name the printer
shortens resolves as it does in the Spec module. Lean Comparator is what
checks the round trip: it compares the challenge's copy of each `_stmt` with
the Spec layer's, constant for constant. -/
def stmtSource (v : DefinitionVal) (width : Nat := 100) : CoreM String :=
  withReader (fun ctx => { ctx with currNamespace := `RueCore.Spec }) <|
    withOptions (fun o => o.setBool `pp.funBinderTypes true) do
      Meta.MetaM.run' do
        return (← Meta.ppExpr v.value).pretty width

/-- (helper) The modules the Spec layer's modules import from L0 and L1: what
the challenge imports, so that it states the spine over those layers alone. -/
def specImports (env : Environment) : Array Name := Id.run do
  let mut out : Array Name := #[]
  for m in env.header.moduleNames do
    unless Layers.layerOf? m == some Layers.specLayer do continue
    let some i := env.getModuleIdx? m | continue
    for imp in env.header.moduleData[i.toNat]!.imports do
      let l := Layers.layerOf? imp.module
      if (l == some 0 || l == some 1) && !out.contains imp.module then
        out := out.push imp.module
  return out.qsort (·.toString < ·.toString)

/-- (helper) Lean Comparator's challenge (`comparator/Challenge.lean`),
generated from `RueCore.Spec.spine`: every Spec statement written out in full
— its elaborated body, pretty-printed, as a `def RueCore.Spec.<name>_stmt :
Prop` of the challenge's own — then each `RueCore.Spine` theorem with that
statement as its type and `sorry` for a proof. It imports L0 and L1 only (the
modules the Spec layer imports), not the Spec layer: Comparator compares
every constant a challenge statement uses with the solution's, the `_stmt`
definitions included, so the challenge pins the statements' content, and a
Spec statement changed without regenerating the challenge fails Comparator,
while one changed and regenerated shows in the challenge's diff. -/
def challengeReport (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --challenge: {p}"
  let mut out : Array String := (specImports env).map (s!"import {·}")
  out := out ++ #[
    "",
    "/-!",
    "# Lean Comparator's challenge (RUE-2460)",
    "",
    "Generated by `lake exe ruecore-digest --challenge` from `RueCore.Spec.spine`,",
    "`RueCore.Spec.witnesses` and `RueCore.Spec.sharpness`; do not edit. It restates every Spec statement in full,",
    "as the kernel holds it (its elaborated body, pretty-printed), over the L0 and L1",
    "modules the Spec layer",
    "imports and nothing else; then one theorem per statement, with the statement as",
    "its type and `sorry` for a proof. `RueCore.Spine`, the solution, proves the same",
    "names with the same types; Comparator checks that every constant these",
    "statements use, each `_stmt` included, is the same in the solution's",
    "environment, so a statement changed in the Spec layer fails Comparator until",
    "this file is regenerated, and the change is then this file's diff.",
    "`comparator/config.json` names the theorems. How to run it: README, \"The",
    "statement layer\".",
    "-/",
    "",
    "set_option autoImplicit false",
    "",
    "namespace RueCore.Spec",
    ""]
  for (h, s) in Lint.entries do
    let some (.defnInfo v) := Lint.find? env s | continue
    let body ← stmtSource v
    out := out ++ #[s!"/-- The statement `{Digest.shortName h}` proves. -/",
      s!"def {s.replacePrefix `RueCore.Spec .anonymous} : Prop :=",
      s!"  {("\n  ".intercalate (body.splitOn "\n"))}", ""]
  out := out ++ #["end RueCore.Spec", "", "namespace RueCore.Spine", ""]
  for (h, s) in Lint.entries do
    out := out.push s!"theorem {Digest.shortName h} : {s} := sorry"
  out := out ++ #["", "end RueCore.Spine", ""]
  return ("\n".intercalate out.toList, if problems.isEmpty then 0 else 1)

/-- (helper) FNV-1a, 64 bits, over a string's UTF-8 bytes: a fingerprint
that is the same on every machine and every run. -/
def fnv1a64 (s : String) : UInt64 := Id.run do
  let mut h : UInt64 := 0xcbf29ce484222325
  for b in s.toUTF8 do
    h := (h ^^^ b.toUInt64) * 0x100000001b3
  return h

/-- (helper) A fingerprint as sixteen hex digits. -/
def hex16 (h : UInt64) : String :=
  let d := (Nat.toDigits 16 h.toNat)
  String.ofList (List.replicate (16 - d.length) '0' ++ d)

/-- (helper) The spine's fingerprints (`spine-fingerprints.txt`): one line
per Spec statement, in `Lint.entries`'s order (the spine, then the witnesses), with a hash of its elaborated
body (the kernel's term, printed with full names and binder names). The file
is committed, and `bin/chain.sh` fails when a regenerated copy differs from
it — a statement added, removed or changed — naming each, so that a change
to what the mechanization claims is regenerated deliberately and reviewed,
not carried along by a proof edit. -/
def fingerprintReport (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --fingerprint: {p}"
  let mut out : Array String := #[
    "# The spine's fingerprints (RUE-2460): a hash of each Spec statement's elaborated body.",
    "# Generated by `lake exe ruecore-digest --fingerprint > spine-fingerprints.txt`; do not",
    "# edit. A changed, added or removed line is a change to what the mechanization claims:",
    "# regenerate only with the Spec diff and SPINE.md under review.",
    s!"# {Lint.entries.length} statements."]
  for (_, s) in Lint.entries do
    let some (.defnInfo v) := Lint.find? env s | continue
    out := out.push s!"{hex16 (fnv1a64 (toString v.value))} {s}"
  return ("\n".intercalate out.toList ++ "\n", if problems.isEmpty then 0 else 1)

/-! ## The proof map (RUE-2468) -/

/-- (helper) One marked node's row of `MAP.md`'s size-stats table: its short
name, its declaring module, its proof size in source lines
(`Map.declLines`), and the distinct unmarked helper theorems `Map.walk`
counted under it before the next marked node. -/
def mapSizeRow (env : Environment) (helperCountOf : NameMap Nat) (n : Name) : CoreM String := do
  let lines ← Map.declLines n
  let helpers := (helperCountOf.find? n).getD 0
  let m := (Lint.moduleOf? env n).getD .anonymous
  return s!"| `{Digest.shortName n}` | `{m}` | {lines} | {helpers} |"

/-- (helper) The spine diagram: every marked node (the spine and the
milestones), in Mermaid subgraphs by declaring module, with an edge `A → B`
for every pair `Map.walkAll` found — so a reader can tell directly from this
diagram which theorems depend on a given marked node, `Step.det` among them
(RUE-2468's acceptance check). -/
def renderSpineDiagram (env : Environment) (markedList : List Name)
    (ancestorsOf : NameMap (Array Name)) : List String := Id.run do
  let mut byModule : Array (Name × Array Name) := #[]
  for n in markedList do
    let m := (Lint.moduleOf? env n).getD .anonymous
    match byModule.findIdx? (·.1 == m) with
    | some i => byModule := byModule.set! i (m, (byModule[i]!).2.push n)
    | none => byModule := byModule.push (m, #[n])
  let sortedModules := byModule.qsort (fun a b => a.1.toString < b.1.toString)
  let mut out : Array String := #["```mermaid", "flowchart BT"]
  for (m, ns) in sortedModules do
    let layer := (Layers.layerOf? m).getD 9
    out := out.push s!"  subgraph {Map.sanitizeId m.toString}[\"{m} ({Lint.layerLabel layer})\"]"
    for n in ns.qsort (fun a b => a.toString < b.toString) do
      out := out.push s!"    {Map.sanitizeId (Digest.shortName n)}[\"{Digest.shortName n}\"]"
    out := out.push "  end"
  let mut edges : Array (Name × Name) := #[]
  for n in markedList do
    for a in (ancestorsOf.find? n).getD #[] do
      edges := edges.push (a, n)
  let sortedEdges := edges.qsort fun (a1, b1) (a2, b2) =>
    Digest.shortName a1 ++ Digest.shortName b1 < Digest.shortName a2 ++ Digest.shortName b2
  for (a, b) in sortedEdges do
    out := out.push s!"  {Map.sanitizeId (Digest.shortName a)} --> {Map.sanitizeId (Digest.shortName b)}"
  out := out.push "```"
  return out.toList

/-- (helper) One spine theorem's own small diagram: its milestone ancestors
(solid edges: proof dependencies `Map.walk` found), and the definitions its
*statement* depends on — the per-statement type-level closure
`Lint.trustedBase` aggregates over the whole spine, computed here for one
statement alone (`Lint.unfoldClosure`, `Lint.readable`), capped at 25 and
annotated with each definition's calculus citations (dashed edges). Answers,
for `check_sound`, RUE-2468's second acceptance question. -/
def renderTheoremDiagram (env : Environment) (milestoneSet : NameSet)
    (h s : Name) (ancestors : Array Name) : CoreM (List String) := do
  let milestoneAnc := (ancestors.filter milestoneSet.contains).qsort (·.toString < ·.toString)
  let closure := match Lint.statementBody? env s with
    | some body => Lint.readable env (Lint.unfoldClosure env body.getUsedConstants)
    | none => #[]
  let cap := 25
  let shown := if closure.size > cap then closure.extract 0 cap else closure
  let citesOf (d : Name) : CoreM (Array String) := do
    let doc := ((← findDocString? env d)).getD ""
    return Map.sectionCitations doc ++ Map.ruleCitations doc
  let mut out : Array String := #["```mermaid", "flowchart BT",
    s!"  thm[\"{Digest.shortName h}\"]"]
  if !milestoneAnc.isEmpty then
    out := out.push "  subgraph mile[\"Milestone ancestors\"]"
    for a in milestoneAnc do
      out := out.push s!"    {Map.sanitizeId (Digest.shortName a)}[\"{Digest.shortName a}\"]"
    out := out.push "  end"
    for a in milestoneAnc do
      out := out.push s!"  {Map.sanitizeId (Digest.shortName a)} --> thm"
  if !shown.isEmpty then
    out := out.push "  subgraph defs_[\"Definitions the statement depends on\"]"
    for d in shown do
      let cites ← citesOf d
      let label := Digest.shortName d ++
        (if cites.isEmpty then "" else "<br/>" ++ String.intercalate ", " cites.toList)
      out := out.push s!"    {Map.sanitizeId (Digest.shortName d)}[\"{label}\"]"
    out := out.push "  end"
    for d in shown do
      out := out.push s!"  {Map.sanitizeId (Digest.shortName d)} -.-> thm"
  out := out.push "```"
  if closure.size > cap then
    out := out.push ""
    out := out.push s!"({closure.size - shown.size} more definitions the statement depends on, past the {cap} cap.)"
    -- the diagram alone answers "which definitions does this cap short of the
    -- full count", but a reader wanting the complete list (RUE-2468's review,
    -- S2) gets it here, collapsed so the 36 per-theorem sections stay skimmable
    out := out.push ""
    out := out.push "<details>"
    out := out.push s!"<summary>All {closure.size} definitions {Digest.shortName h}'s statement depends on</summary>"
    out := out.push ""
    for d in closure do
      let cites ← citesOf d
      let suffix := if cites.isEmpty then "" else " — " ++ String.intercalate ", " cites.toList
      out := out.push s!"- `{Digest.shortName d}`{suffix}"
    out := out.push ""
    out := out.push "</details>"
  return out.toList

/-- (helper) `MAP.md` (RUE-2468): the spine diagram, one small diagram per
spine theorem, the static assurance-chain diagram, and the size-stats table
— all computed from the compiled environment (`RueCore.Map`), never by
grepping. -/
def mapReport (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env ++ Map.milestoneProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --map: {p}"
  let markedList := Map.marked
  let milestoneSet := Map.milestoneNames.foldl (init := NameSet.empty) (·.insert ·)
  let (ancestorsOf, helperCountOf) ← Map.walkAll env markedList
  let mut out : Array String := #[
    "# The proof map",
    "",
    "<!-- Generated by `lake exe ruecore-digest --map`; do not edit by hand. -->",
    "",
    "How the mechanization's theorems hang together: which marked node's proof",
    "rests on which, what each spine theorem's statement unfolds to, and where the",
    "proof chain sits beside the compiler bridge. `README.md`, \"The proof map\",",
    "explains what a **spine node** and a **milestone lemma** are and how this file",
    "is generated. GitHub renders every diagram below inline.",
    "",
    s!"{Lint.headline.length} spine theorems, {Map.milestones.length} milestone lemmas: " ++
      s!"{markedList.length} marked nodes in all.",
    "",
    "## The spine",
    "",
    "Every marked node, grouped by the module it is declared in, with an edge",
    "`A → B` when `B`'s proof uses `A` transitively through helper theorems that",
    "are not themselves marked. From this diagram alone: every theorem with an",
    "incoming edge from `Step.det` depends on it.",
    ""]
  out := out ++ (renderSpineDiagram env markedList ancestorsOf).toArray ++ #[""]
  out := out ++ #["## Milestone lemmas", "",
    "The load-bearing lemmas besides the spine (`RueCore/Map.lean`'s `milestones`",
    "list), each with its one-line reason, its proof size, and the unmarked",
    "helper theorems `Map.walk` counted under it before the next marked node:",
    "",
    "| Milestone | Reason | Proof lines | Unmarked helpers under it |",
    "| --- | --- | --- | --- |"]
  for (n, reason) in Map.milestones do
    let lines ← Map.declLines n
    let helpers := (helperCountOf.find? n).getD 0
    out := out.push s!"| `{Digest.shortName n}` | {reason} | {lines} | {helpers} |"
  out := out ++ #["", "## Per-spine-theorem diagrams", "",
    "One small diagram per spine theorem: its milestone ancestors, and the",
    "definitions its statement depends on (the per-statement trusted-base",
    "closure, `Lint.trustedBase`'s aggregate computed here one statement at a",
    "time), each with the `§N.M` and `(Rule-Name)` citations its doc-comment",
    "carries. Capped at 25 definitions; a capped diagram says how many more there",
    "were.", ""]
  for (h, s) in Spec.spine do
    let ancestors := (ancestorsOf.find? h).getD #[]
    out := out.push s!"### `{Digest.shortName h}`"
    out := out.push ""
    out := out ++ (← renderTheoremDiagram env milestoneSet h s ancestors).toArray
    out := out.push ""
  out := out ++ #["## The assurance chain", "",
    "Static: what the proof chain covers, and how the bridge corpus tests the",
    "compiler against the same model — kept in content beside",
    "`../WHAT-IT-MEANS.md`'s diagram (RUE-2462) without depending on that file.",
    ""]
  out := out ++ Map.assuranceChainDiagram.toArray ++ #[""]
  out := out ++ #["## Size stats", "",
    "Proof size in source lines (from the declaration's range), and the number of",
    "distinct unmarked helper theorems `Map.walk` counted under it before the next",
    "marked node.", "",
    "| Marked node | Module | Proof lines | Unmarked helpers under it |",
    "| --- | --- | --- | --- |"]
  for n in markedList do
    out := out.push (← mapSizeRow env helperCountOf n)
  out := out.push ""
  return ("\n".intercalate out.toList, if problems.isEmpty then 0 else 1)

/-- (helper) Lean Comparator's configuration (`comparator/config.json`):
the challenge and solution modules, the theorems to compare — one per Spec
statement — and the axioms this project allows. -/
def comparatorConfig (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --comparator-config: {p}"
  let names := Lint.entries.map fun (h, _) => Json.str (toString (Lint.spineName h))
  let json := Json.mkObj [
    ("challenge_module", "Challenge"),
    ("solution_module", "RueCore.Spine"),
    ("theorem_names", Json.arr names.toArray),
    ("permitted_axioms", Json.arr (Lint.allowedAxioms.map (Json.str ∘ toString)).toArray),
    ("enable_nanoda", false)]
  return (json.pretty ++ "\n", if problems.isEmpty then 0 else 1)

/-- (helper) Import the mechanization and run one report against the imported
environment, with names printed as the sources write them (the `RueCore`
namespace is open). -/
unsafe def withEnvironment (act : Environment → CoreM (String × UInt32)) : IO UInt32 := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules #[{ module := `RueCore }] {} (trustLevel := 1024) (loadExts := true)
  let ctx : Core.Context :=
    { fileName := "<ruecore-digest>", fileMap := default, currNamespace := `RueCore }
  let ((text, code), _) ← (act env).toIO ctx { env := env }
  IO.print text
  return code

/-- (helper) The digest, with the scope section quoted from the generated
index at `path`. The index is read before the environment is imported, so a
missing one is a sentence rather than an exception. -/
unsafe def digestWithIndex (path : String) : IO UInt32 := do
  if !(← System.FilePath.pathExists path) then
    IO.eprintln s!"ruecore-digest: cannot read {path}"
    IO.eprintln "the digest quotes INDEX.md's coverage lines for its scope section; \
      run from docs/formal/lean, or pass --index <path>"
    return 1
  withEnvironment (digestReport (← IO.FS.readFile path))

/-- (helper) Print one of the two reports on stdout. -/
unsafe def mainUnsafe (args : List String) : IO UInt32 := do
  match args with
  | [] => digestWithIndex "INDEX.md"
  | ["--index", path] => digestWithIndex path
  | ["--trust"] => withEnvironment trustReport
  | ["--spine"] => withEnvironment spineReport
  | ["--challenge"] => withEnvironment challengeReport
  | ["--comparator-config"] => withEnvironment comparatorConfig
  | ["--fingerprint"] => withEnvironment fingerprintReport
  | ["--map"] => withEnvironment mapReport
  | _ => IO.eprintln usage; pure 1

/-- (helper) The entry point's safe face. -/
@[implemented_by mainUnsafe] opaque mainImpl (args : List String) : IO UInt32

/-- (helper) `lake exe ruecore-digest`: the statement digest, or, with
`--trust`, the trust report. -/
def main (args : List String) : IO UInt32 := mainImpl args
