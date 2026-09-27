import RueCore.Map

/-!
# simplify_metrics — the environment's half of `bin/simplify-metrics.py` (RUE-2471)

A helper of `bin/simplify-metrics.py`, run with `lake env lean --run` against a built package.
It is L3 tooling: not a module of the package, imported by nothing, and outside every root the
layering audit and the trusted-base lint walk. It imports the library root `RueCore` and
prints one JSON object on standard output, with three parts:

* `theorems`: every authored theorem of the package, with its module and its source lines (from
  the declaration range the compiled module recorded). Authored means declared in a package
  module, with a declaration range, and not a compiler-generated name (an equation lemma, a
  `match` or `proof_` auxiliary, a structure's `Prop` field projection). A `private` theorem counts,
  under its user-facing name.
* `spine`: for each spine theorem (`RueCore.Lint.headline`, i.e. `RueCore.Spec.spine`), the
  authored theorems its proof depends on transitively, found by walking proof terms the way
  `RueCore.Map.walk` does (`RueCore.Map.proofValue?`: a theorem's proof, and the value of a
  proof-bearing or compiler-generated `def` in between), staying inside the package. Three
  counts: `all`, every authored non-spine theorem reached, walking through other spine
  theorems too; `own`, the same but stopping at another spine theorem (whose own helpers are
  then not counted); and `map`, the count `MAP.md`'s size stats print, which stops at every
  marked node (spine and milestones) and is computed here by `RueCore.Map.walkAll` itself.
* `exact` and `skeleton`: groups of authored theorems with the same statement shape. Each
  theorem's type is normalized: universe parameters renamed by position, binder names and
  binder annotations erased, metadata stripped; α-equivalence is then `Expr` equality, and
  the groups are the hash buckets (checked with `==`) holding two or more names. `exact`
  compares the normalized type itself. `skeleton` first replaces every package constant that
  is a predicate (its type ends in `Prop`) by one placeholder, so two lemmas that say the same
  thing about different predicates (`Tidy.bind` and a `Settled.bind` of the same form, say)
  fall in one group.
-/

open Lean Meta

namespace SimplifyMetrics

/-- A private name's user-facing form, and every other name unchanged. -/
def userName (n : Name) : Name := (privateToUserName? n).getD n

/-- The last string component of a name, or `""`. -/
def lastStr : Name → String
  | .str _ s => s
  | .num p _ => lastStr p
  | .anonymous => ""

/-- Is this last component one Lean generates for an authored declaration (equation lemmas,
unfolding lemmas, functional induction, structure eta)? -/
def generatedSuffix (s : String) : Bool :=
  (s.startsWith "eq_" && (s.drop 3).all Char.isDigit && s.length > 3) || s == "eq_def" || s == "eq_unfold" || s == "induct" ||
    s == "induct_unfolding" || s == "mutual_induct" || s == "fun_cases" || s == "eta" ||
    s == "ext_iff" || s == "inj" || s == "injEq" || s == "sizeOf_spec"

/-- Is `n` an authored theorem of the package? -/
def authoredThm (env : Environment) (n : Name) (ci : ConstantInfo) : CoreM Bool := do
  unless ci matches .thmInfo _ do return false
  unless RueCore.Lint.inPackage env n do return false
  let u := userName n
  if u.isInternalDetail then return false
  if generatedSuffix (lastStr u) then return false
  if env.isProjectionFn n then return false
  return (← findDeclarationRanges? n).isSome

/-- Every theorem of the package (authored or generated) reachable from `root`'s proof, walking
through proof-bearing `def`s (`RueCore.Map.proofValue?`), and, unless `stop` holds, through the
proofs of the `spine` theorems it meets too. -/
def reach (env : Environment) (spine : NameSet) (stop : Bool) (root : Name) :
    CoreM NameSet := do
  let mut seen : NameSet := NameSet.empty.insert root
  let mut out : NameSet := {}
  let mut stack : Array Name := #[root]
  while !stack.isEmpty do
    let n := stack.back!
    stack := stack.pop
    let some v ← RueCore.Map.proofValue? env n | continue
    for c in v.getUsedConstants do
      if seen.contains c || !RueCore.Lint.inPackage env c then continue
      seen := seen.insert c
      match RueCore.Lint.find? env c with
      | some (.thmInfo _) =>
          out := out.insert c
          unless stop && spine.contains c do stack := stack.push c
      | some (.defnInfo _) => stack := stack.push c
      | _ => pure ()
  return out

/-- Binder names, binder annotations and metadata erased: `Expr` equality is then
α-equivalence up to implicit/explicit binders. -/
partial def normE : Lean.Expr → Lean.Expr
  | .forallE _ t b _ => .forallE `_ (normE t) (normE b) .default
  | .lam _ t b _ => .lam `_ (normE t) (normE b) .default
  | .letE _ t v b nd => .letE `_ (normE t) (normE v) (normE b) nd
  | .mdata _ e => normE e
  | .app f a => .app (normE f) (normE a)
  | .proj s i e => .proj s i (normE e)
  | e => e

/-- Is this constant a predicate of the package: its type, under its binders, is `Prop`? -/
def isPred (env : Environment) (c : Name) : Bool :=
  RueCore.Lint.inPackage env c &&
    match RueCore.Lint.find? env c with
    | some ci => ci.type.getForallBody.isProp && !(ci matches .thmInfo _)
    | none => false

/-- Every package predicate replaced by the placeholder `P`. -/
def skeleton (env : Environment) (e : Lean.Expr) : Lean.Expr :=
  e.replace fun
    | .const c _ => if isPred env c then some (.const `P []) else none
    | _ => none

/-- The number of nodes of an expression, shared subterms counted each time. -/
partial def exprSize : Lean.Expr → Nat
  | .forallE _ t b _ | .lam _ t b _ => 1 + exprSize t + exprSize b
  | .letE _ t v b _ => 1 + exprSize t + exprSize v + exprSize b
  | .mdata _ e | .proj _ _ e => 1 + exprSize e
  | .app f a => 1 + exprSize f + exprSize a
  | _ => 1

/-- A normalized type: universe parameters renamed by position, then `normE`. -/
def normType (ci : ConstantInfo) : Lean.Expr :=
  let us := (List.range ci.levelParams.length).map fun i => Level.param (Name.mkSimple s!"u{i}")
  normE (ci.type.instantiateLevelParams ci.levelParams us)

/-- The groups of two or more names whose keys are equal, each group's names sorted, the
groups sorted by their first name. -/
def groups (keyed : Array (Name × Lean.Expr)) : Array (Array Name) := Id.run do
  let mut buckets : Std.HashMap UInt64 (Array (Lean.Expr × Array Name)) := {}
  for (n, e) in keyed do
    let h := e.hash
    let bs := buckets.getD h #[]
    match bs.findIdx? (·.1 == e) with
    | some i => buckets := buckets.insert h (bs.modify i fun (e', ns) => (e', ns.push n))
    | none => buckets := buckets.insert h (bs.push (e, #[n]))
  let mut out : Array (Array Name) := #[]
  for (_, bs) in buckets.toList do
    for (_, ns) in bs do
      if ns.size ≥ 2 then
        out := out.push (ns.qsort (·.toString < ·.toString))
  return out.qsort (fun a b => a[0]!.toString < b[0]!.toString)

def str (n : Name) : Json := Json.str (userName n).toString

def run (env : Environment) : CoreM Json := do
  let mut thms : Array (Name × ConstantInfo) := #[]
  for (n, ci) in env.constants.toList do
    if ← authoredThm env n ci then thms := thms.push (n, ci)
  thms := thms.qsort (fun a b => (userName a.1).toString < (userName b.1).toString)
  let authored : NameSet := thms.foldl (init := {}) fun s (n, _) => s.insert n
  let mut thmJson : Array Json := #[]
  for (n, _) in thms do
    let lines ← RueCore.Map.declLines n
    let m := (RueCore.Lint.moduleOf? env n).getD .anonymous
    thmJson := thmJson.push <| Json.mkObj
      [("name", str n), ("module", Json.str m.toString), ("lines", toJson lines)]
  let headline := RueCore.Lint.headline
  let spineSet : NameSet := headline.foldl (init := {}) (·.insert ·)
  let (_, mapCount) ← RueCore.Map.walkAll env RueCore.Map.marked
  let mut spineJson : Array Json := #[]
  for s in headline do
    let all ← reach env spineSet false s
    let own ← reach env spineSet true s
    let count (xs : NameSet) : Nat :=
      xs.toList.foldl (init := 0) fun k c => if authored.contains c && !spineSet.contains c then k + 1 else k
    let spinesMet := own.toList.foldl (init := 0) fun k c => if spineSet.contains c then k + 1 else k
    let helperLines ← all.toList.foldlM (init := 0) fun k c =>
      if authored.contains c && !spineSet.contains c then
        return k + (← RueCore.Map.declLines c)
      else return k
    let m := (RueCore.Lint.moduleOf? env s).getD .anonymous
    spineJson := spineJson.push <| Json.mkObj
      [("name", str s), ("module", Json.str m.toString), ("lines", toJson (← RueCore.Map.declLines s)),
       ("all", toJson (count all)), ("allLines", toJson helperLines), ("own", toJson (count own)),
       ("spineMet", toJson spinesMet), ("map", toJson (mapCount.find? s |>.getD 0))]
  let keyed := thms.map fun (n, ci) => (n, normType ci)
  let exact := groups keyed
  let skel := groups (keyed.map fun (n, e) => (n, skeleton env e))
  let enc (gs : Array (Array Name)) (ty : Name → Lean.Expr) : Json :=
    Json.arr <| gs.map fun g => Json.mkObj
      [("names", Json.arr (g.map str)), ("size", toJson (exprSize (ty g[0]!)))]
  let tyOf : NameMap Lean.Expr := keyed.foldl (init := {}) fun m (n, e) => m.insert n e
  let ty (n : Name) : Lean.Expr := tyOf.find? n |>.getD (.bvar 0)
  return Json.mkObj [("theorems", Json.arr thmJson), ("spine", Json.arr spineJson),
    ("exact", enc exact ty), ("skeleton", enc skel ty)]

unsafe def main (_ : List String) : IO UInt32 := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules #[{ module := `RueCore }, { module := `RueCore.Map }] {}
    (trustLevel := 1024) (loadExts := true)
  let ctx : Core.Context :=
    { fileName := "simplify_metrics", fileMap := default, maxHeartbeats := 0 }
  let (j, _) ← (run env).toIO ctx { env }
  IO.println j.compress
  return 0

end SimplifyMetrics

unsafe def main (args : List String) : IO UInt32 := SimplifyMetrics.main args
