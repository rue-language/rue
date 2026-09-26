import Lean

/-!
# mutate_polarity — where a definition occurs in the package's statements (RUE-2499)

A helper of `bin/mutate.py`, run with `lake env lean --run` against a built package (it is not
a module of the package, and nothing imports it). It reads the environment of every proof
module (`RueCore.Sharp.Glue` and `RueCore.Nonvacuous.Glue` import all of L0, L1, the Spec
layer and L2) and answers two questions, as JSON on standard output:

* `ranges`: every declaration of the package with its module and source lines, so that
  `mutate.py` can name the definitions a mutant's edits fall in (its *targets*) and the
  theorem each `theorem` keyword of a source file declares.
* `analyze TARGET…`: for every theorem of the package and each target, whether its statement
  involves the target at all (the target, or a definition that unfolds to it, occurs in the
  statement), and at which polarity: `+` (a conclusion: a weaker target makes the statement
  weaker), `-` (a hypothesis, or under `¬`: a weaker target makes the statement *stronger*,
  so it can become false), `±` (both sides of an `↔`, or inside a term — an argument of `=`,
  a function's body — where no direction is known). For the statements `mutate.py` reads
  (`RueCore.Spine.*`, `RueCore.Sharp.Glue.*`, `RueCore.Nonvacuous.Glue.*`) it also gives each
  occurrence's position (`hypothesis`, `¬`, `↔`, `term`, outermost first; none is a
  conclusion) and the definitions unfolded to reach it.

The walk unfolds a `Prop`-valued definition of the package through its body and a
`Prop`-valued inductive (or structure) through its constructors' premises, which occur at the
inductive's own polarity (a weaker premise admits more derivations). Anything it does not
read structurally (an application of a function it does not know, a `match` discriminant, a
recursive definition compiled to `brecOn`, an argument of `=`) is scanned as a term: every
target that any constant in it unfolds to occurs there at `±`. So the answer is conservative:
a target it reports at `+` only really occurs only in conclusions, and one it does not report
does not occur at all.
-/

open Lean Meta

namespace MutatePolarity

inductive Pol | pos | neg | mix
  deriving BEq, Hashable, Inhabited

def Pol.flip : Pol → Pol
  | .pos => .neg | .neg => .pos | .mix => .mix

/-- The polarity of an occurrence at `inner` inside a position of polarity `outer`. -/
def Pol.comp : Pol → Pol → Pol
  | .pos, p => p | .neg, p => p.flip | .mix, _ => .mix

def Pol.str : Pol → String
  | .pos => "+" | .neg => "-" | .mix => "±"

structure Occ where
  target : Name
  pol : Pol
  labels : List String
  path : List Name
  deriving BEq, Hashable, Inhabited

def Occ.under (o : Occ) (p : Pol) (label : String) : Occ :=
  { o with pol := p.comp o.pol, labels := label :: o.labels }

def Occ.via (o : Occ) (c : Name) : Occ := { o with path := c :: o.path }

structure Ctx where
  /-- The target a constant belongs to (the target itself, or one of its auxiliary
  declarations: a constructor, an equation lemma, a `match`). -/
  targetOf : Name → Option Name
  /-- The targets a constant unfolds to, not counting its own. -/
  taint : Std.HashMap Name (Array Name)

structure St where
  memo : Std.HashMap Name (Array Occ) := {}
  inProgress : NameSet := {}
  /-- The constants whose unfolding was cut (being unfolded further up) since the innermost
  `unfold` began. -/
  cuts : NameSet := {}

abbrev M := ReaderT Ctx (StateRefT St MetaM)

def dedup (os : Array Occ) : Array Occ := Id.run do
  let mut seen : Std.HashSet (Name × Pol × List String) := {}
  let mut out := #[]
  for o in os do
    let k := (o.target, o.pol, o.labels)
    unless seen.contains k do
      seen := seen.insert k
      out := out.push o
  return out

/-- Every target a term mentions, at `±`. -/
def termScan (e : Expr) : M (Array Occ) := do
  let ctx ← read
  let mut out := #[]
  for k in e.getUsedConstants do
    if let some t := ctx.targetOf k then
      out := out.push { target := t, pol := .mix, labels := ["term"], path := [] }
    for t in ctx.taint.getD k #[] do
      out := out.push { target := t, pol := .mix, labels := ["term"], path := [k] }
  return dedup out

mutual

/-- The occurrences of the targets in a proposition `e`, at polarity relative to `e`. -/
partial def walk (e : Expr) : M (Array Occ) := do
  let e := e.consumeMData.headBeta
  match e with
  | .forallE n ty body bi =>
    withLocalDecl n bi ty fun x => do
      let b ← walk (body.instantiate1 x)
      if ← isProp ty then
        let a ← walk ty
        return dedup (a.map (·.under .neg "hypothesis") ++ b)
      else
        return dedup ((← termScan ty) ++ b)
  | _ =>
    let fn := e.getAppFn
    let args := e.getAppArgs
    let .const c _ := fn | termScan e
    if c == ``Not && args.size == 1 then
      return (← walk args[0]!).map (·.under .neg "¬")
    if (c == ``And || c == ``Or) && args.size == 2 then
      return dedup ((← walk args[0]!) ++ (← walk args[1]!))
    if c == ``Iff && args.size == 2 then
      return dedup (((← walk args[0]!) ++ (← walk args[1]!)).map (·.under .mix "↔"))
    if c == ``True || c == ``False then
      return #[]
    if c == ``Exists && args.size == 2 then
      let tyOcc ← termScan args[0]!
      match args[1]! with
      | .lam n ty b bi =>
        return dedup (tyOcc ++ (← withLocalDecl n bi ty fun x => walk (b.instantiate1 x)))
      | p => return dedup (tyOcc ++ (← termScan p))
    -- A `match` is read through its alternatives. This comes before the target test: a
    -- target's own `match` is one of its auxiliary declarations, met only while unfolding the
    -- target, and `unfold` leaves a target's own occurrences out.
    if let some mapp ← matchMatcherApp? e then
      if mapp.remaining.isEmpty then
        let mut out := #[]
        for d in mapp.discrs do out := out ++ (← termScan d)
        for p in mapp.params do out := out ++ (← termScan p)
        for alt in mapp.alts do
          out := out ++ (← lambdaTelescope alt fun _ body => do
            if ← isProp body then walk body else termScan body)
        return dedup out
    let ctx ← read
    -- A target records its own occurrence here, and is unfolded like any other constant for
    -- the other targets it may unfold to (`unfold` leaves out its own).
    let mut out : Array Occ := match ctx.targetOf c with
      | some t => #[{ target := t, pol := .pos, labels := [], path := [] }]
      | none => #[]
    for a in args do out := out ++ (← termScan a)
    if ctx.taint.contains c then
      let inner ← unfold c
      out := out ++ inner.map (·.via c)
    return dedup out

/-- The occurrences of the targets in a `Prop`-valued constant of the package: a definition
through its body, an inductive through its constructors' premises. A constant already being
unfolded further up (a recursive or mutual inductive) contributes nothing here, since its
occurrences are collected there; so a result is memoized only when no constant but `c` itself
was cut on the way, and is otherwise recomputed at each use. -/
partial def unfold (c : Name) : M (Array Occ) := do
  if let some r := (← get).memo[c]? then return r
  if (← get).inProgress.contains c then
    modify fun s => { s with cuts := s.cuts.insert c }
    return #[]
  let outer := (← get).cuts
  modify fun s => { s with inProgress := s.inProgress.insert c, cuts := {} }
  let env ← getEnv
  let res : Array Occ ← match env.find? c with
    | some (.defnInfo d) =>
      lambdaTelescope d.value fun _ body => do
        if ← isProp body then walk body else termScan body
    | some (.inductInfo iv) =>
      let mut out := #[]
      for ctor in iv.ctors do
        let some (.ctorInfo cv) := env.find? ctor | continue
        out := out ++ (← forallTelescope cv.type fun xs concl => do
          let mut o := #[]
          for x in xs do
            let ty ← inferType x
            if ← isProp ty then o := o ++ (← walk ty) else o := o ++ (← termScan ty)
          for a in concl.getAppArgs do o := o ++ (← termScan a)
          return o)
      pure (dedup out)
    | some ci => termScan ci.type
    | none => pure #[]
  -- A target's own body (its constructors, its `match`es) is what the mutant changes, not an
  -- occurrence of it.
  let res := match (← read).targetOf c with
    | some t => res.filter (·.target != t)
    | none => res
  let cuts := (← get).cuts.erase c
  modify fun s => { s with inProgress := s.inProgress.erase c,
                           cuts := cuts.foldl (·.insert ·) outer }
  if cuts.isEmpty then
    modify fun s => { s with memo := s.memo.insert c res }
  return res

end

def isPkg (env : Environment) (n : Name) : Bool :=
  match env.getModuleIdxFor? n with
  | some i => (`RueCore).isPrefixOf env.header.moduleNames[i.toNat]!
  | none => false

def moduleOf (env : Environment) (n : Name) : String :=
  match env.getModuleIdxFor? n with
  | some i => env.header.moduleNames[i.toNat]!.toString
  | none => ""

/-- The constants a constant's meaning depends on: its type, a definition's body, an
inductive's constructors. A theorem's proof is left out: by proof irrelevance, changing a
proof never changes what a definition means. -/
def deps (ci : ConstantInfo) : Array Name :=
  match ci with
  | .defnInfo d => (d.type.getUsedConstants ++ d.value.getUsedConstants)
  | .inductInfo iv => iv.type.getUsedConstants ++ iv.ctors.toArray
  | .ctorInfo cv => cv.type.getUsedConstants.push cv.induct
  | .opaqueInfo o => o.type.getUsedConstants ++ o.value.getUsedConstants
  | ci => ci.type.getUsedConstants

def jstr (s : String) : String := (Json.str s).compress

def rangeJson (env : Environment) (n : Name) (kind : String) (r : DeclarationRanges) : String :=
  s!"\{\"name\":{jstr n.toString},\"kind\":{jstr kind},\"module\":{jstr (moduleOf env n)}," ++
  s!"\"start\":{r.range.pos.line},\"end\":{r.range.endPos.line},\"sel\":{r.selectionRange.pos.line}}"

def kindOf : ConstantInfo → String
  | .thmInfo _ => "theorem" | .defnInfo _ => "def" | .inductInfo _ => "inductive"
  | .ctorInfo _ => "ctor" | .axiomInfo _ => "axiom" | .opaqueInfo _ => "opaque"
  | .recInfo _ => "rec" | .quotInfo _ => "quot"

def statementPrefixes : List Name := [`RueCore.Spine, `RueCore.Sharp.Glue, `RueCore.Nonvacuous.Glue]

def occJson (o : Occ) : String :=
  s!"[{jstr o.target.toString},{jstr o.pol.str},{(Json.arr (o.labels.toArray.map Json.str)).compress}," ++
  s!"{(Json.arr (o.path.toArray.map (Json.str ∘ Name.toString))).compress}]"

unsafe def main (args : List String) : IO UInt32 := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules #[{ module := `RueCore.Sharp.Glue }, { module := `RueCore.Nonvacuous.Glue }] {}
    (loadExts := true)
  let pkg : Array (Name × ConstantInfo) := env.constants.map₁.fold (init := #[]) fun acc n ci =>
    if isPkg env n then acc.push (n, ci) else acc
  let ctx : Core.Context := { fileName := "mutate_polarity", fileMap := default, maxHeartbeats := 0 }
  match args with
  | ["ranges"] =>
    let (lines, _) ← (do
      let mut out := #[]
      for (n, ci) in pkg do
        if n.isInternal then continue
        if let some r ← findDeclarationRanges? n then
          out := out.push (rangeJson env n (kindOf ci) r)
      return out : CoreM (Array String)).toIO ctx { env }
    IO.println ("[" ++ ",\n".intercalate lines.toList ++ "]")
    return 0
  | "analyze" :: targets =>
    let targets := targets.map String.toName
    -- The constants of each target: the target and every package constant it prefixes, but
    -- not a `match` auxiliary. Lean reuses one `match` for every later `match` of the same
    -- shape (`Config.trace` uses `Config.Ordered.match_1`), and a `match` means the same case
    -- analysis wherever it is used, so it carries no target into another definition.
    let targetOf : Name → Option Name := fun k =>
      if Meta.isMatcherCore env k then none else targets.find? (·.isPrefixOf k)
    -- Reverse dependencies over the package's non-theorem constants, then, for each target,
    -- everything that reaches it.
    let mut rdeps : Std.HashMap Name (Array Name) := {}
    for (n, ci) in pkg do
      if ci matches .thmInfo _ then continue
      for d in deps ci do
        if d != n && isPkg env d then
          rdeps := rdeps.insert d ((rdeps.getD d #[]).push n)
    let mut taint : Std.HashMap Name (Array Name) := {}
    for t in targets do
      let mut stack : Array Name := (pkg.filter (fun (n, _) => targetOf n == some t)).map (·.1)
      let mut seen : NameSet := stack.foldl (·.insert ·) {}
      while h : stack.size > 0 do
        let k := stack.back
        stack := stack.pop
        for r in rdeps.getD k #[] do
          unless seen.contains r do
            seen := seen.insert r
            stack := stack.push r
            if targetOf r != some t then
              taint := taint.insert r ((taint.getD r #[]).push t)
    let c : Ctx := { targetOf, taint }
    let (lines, _) ← ((do
      let mut out := #[]
      for (n, ci) in pkg do
        let .thmInfo tv := ci | continue
        if n.isInternal then continue
        let occs ← withReducible (walk tv.type)
        let stmt := statementPrefixes.any (·.isPrefixOf n)
        -- Per target, the polarities at which it occurs; the full occurrences for statements.
        let mut pols : Std.HashMap Name (Array String) := {}
        for o in occs do
          let ps := pols.getD o.target #[]
          unless ps.contains o.pol.str do pols := pols.insert o.target (ps.push o.pol.str)
        -- A cross-check of the walk: the targets the statement's constants unfold to.
        let mut inv : Array Name := #[]
        for k in tv.type.getUsedConstants do
          if let some t := targetOf k then
            unless inv.contains t do inv := inv.push t
          for t in taint.getD k #[] do
            unless inv.contains t do inv := inv.push t
        let polsJ := ",".intercalate (pols.toList.map fun (t, ps) =>
          s!"{jstr t.toString}:{(Json.arr (ps.map Json.str)).compress}")
        let invJ := (Json.arr (inv.map (Json.str ∘ Name.toString))).compress
        let occJ := if stmt then "[" ++ ",".intercalate (occs.toList.map occJson) ++ "]" else "[]"
        out := out.push (s!"\{\"name\":{jstr n.toString},\"module\":{jstr (moduleOf env n)}," ++
          s!"\"pols\":\{{polsJ}},\"involves\":{invJ},\"occ\":{occJ}}")
      return out : M (Array String)).run c |>.run' {} |>.run' {} |>.toIO ctx { env })
    IO.println ("[" ++ ",\n".intercalate lines.toList ++ "]")
    return 0
  | ["axioms"] =>
    -- `#print axioms` of every statement, restricted to `mutate.py`'s markers (the axioms
    -- whose name contains `mutate`), with one memo shared by all statements: a constant's
    -- markers are those of every constant its type and value (a proof included) use.
    let mut memo : Std.HashMap Name (Array Name) := {}
    let mut lines := #[]
    for (n, ci) in pkg do
      unless ci matches .thmInfo _ do continue
      unless statementPrefixes.any (·.isPrefixOf n) do continue
      -- An explicit stack in post-order, so a deep dependency chain cannot overflow.
      let mut stack : Array (Name × Bool) := #[(n, false)]
      while h : stack.size > 0 do
        let (k, done) := stack.back
        stack := stack.pop
        if memo.contains k then continue
        let some kc := env.find? k | memo := memo.insert k #[]; continue
        let ds := kc.type.getUsedConstants ++ ((kc.value? (allowOpaque := true)).map (·.getUsedConstants)).getD #[]
        if done then
          let mut acc : Array Name := if (kc matches .axiomInfo _) && (k.toString.splitOn "mutate").length > 1
            then #[k] else #[]
          for d in ds do
            for x in memo.getD d #[] do
              unless acc.contains x do acc := acc.push x
          memo := memo.insert k acc
        else
          stack := stack.push (k, true)
          for d in ds do
            unless memo.contains d do stack := stack.push (d, false)
      let axs := memo.getD n #[]
      lines := lines.push (s!"{jstr n.toString}:" ++
        (Json.arr (axs.map (Json.str ∘ Name.toString))).compress)
    IO.println ("{" ++ ",\n".intercalate lines.toList ++ "}")
    return 0
  | _ =>
    IO.eprintln "usage: lake env lean --run bin/mutate_polarity.lean (ranges | analyze TARGET… | axioms)"
    return 2

end MutatePolarity

unsafe def main (args : List String) : IO UInt32 := MutatePolarity.main args
