import Lean
import RueCore.Lint

/-!
# RueCore.Map — the proof map (RUE-2468)

`lake exe ruecore-digest --map` (`RueCore/DigestMain.lean`) writes `MAP.md`:
generated Mermaid diagrams of how the mechanization's theorems hang together,
computed from the compiled environment — never by grepping, so the diagrams
cannot drift from what the kernel checked the way a hand-drawn picture could.

**Marked nodes.** Two lists, both closed over the environment:

* the **spine** — `RueCore.Spec.spine`'s 36 headline theorems (RUE-2460),
  read here as `Lint.headline`; this file adds no attribute to a spine
  module, and no edit to one;
* the **milestones** (`milestones` below) — about twenty to thirty load-bearing
  internal lemmas besides the spine: the preservation invariants, the
  `eval`/`Step` simulation lemmas, and the key trace lemmas, each with a
  one-line reason in its list entry's comment. `milestoneProblems` fails the
  generator when a listed name is not a theorem of the environment.

**Edges.** `A → B` when `B`'s proof (its elaborated value, not its type) uses
`A`, transitively through helper theorems that are not themselves marked:
`walk` walks the constants a marked node's proof term mentions, follows an
unmarked theorem's own proof the same way, and stops — recording the edge
instead of recursing — at the first marked theorem each path meets. It is
memoized (`RueCore.Lint`'s `axiomsOf` pass is the same shape, for axioms
instead of marked ancestors), so every theorem's proof is walked once however
many marked nodes reach it. The same pass counts, per marked node, the
distinct unmarked helper theorems it walks through before the next marked
node — the size stat the simplification issue (RUE-2468's "Size stats") uses.

**What `MAP.md` renders**, all in `RueCore/DigestMain.lean`'s `mapReport`:

* the **spine diagram** — every marked node and the edges among them, in
  Mermaid subgraphs by declaring module;
* one small diagram **per spine theorem** — its milestone ancestors (solid
  edges), and the definitions its *statement* depends on (dashed edges): the
  same per-statement type-level closure `RueCore.Lint.trustedBase` aggregates
  over the whole spine, computed here for one statement
  (`Lint.unfoldClosure`, `Lint.readable`), capped at a readable number and
  annotated with the section and parenthesized-rule-label citations its
  doc-comment carries (`sectionCitations`, `ruleCitations`);
* the **assurance-chain diagram** (`assuranceChainDiagram`), static: what the
  proof chain and the bridge each cover, kept in content beside
  `../WHAT-IT-MEANS.md`'s diagram (RUE-2462) without depending on that file;
* the **size stats** table — proof lines from declaration ranges, and the
  helper count `walk` already computed.
-/

open Lean

namespace RueCore.Map

/-! ## Milestone lemmas -/

/-- (helper) The milestone lemmas: load-bearing internal lemmas besides the
36-theorem spine (`Lint.headline`), picked from the preservation invariants,
the `eval`/`Step` simulation lemmas, and the key trace lemmas — the three
groups RUE-2468 asks for. Each entry's second component is its one-line
reason, kept as data (not only a source comment) so `mapReport` can print it
in `MAP.md`'s "Milestone lemmas" table. `milestoneProblems` fails the
generator when a listed name is not a theorem of the environment; nothing
here is a spine theorem already (the two lists are disjoint, so the map's
marked set has no duplicate node). -/
def milestones : List (Name × String) := [
  -- preservation invariants (Statics/Lemmas.lean, Soundness.lean)
  (``Typed.wf, "every derivation preserves the state-shape invariant `OwnSt.wf`, which the join and the loop lemmas below all lean on"),
  (``Ctx.join_absorb, "the join's absorption law: re-entering a loop at its head with the same body derivation is sound (`LoopHead.backEdge`'s proof)"),
  (``Ctx.joinAll_perm, "the §5.5 n-way join fold is invariant under a permutation of the match arms it folds"),
  (``LoopHead.enter, "a loop body is typed at its head state on first entry"),
  (``LoopHead.backEdge, "a loop body re-typed at its head state after one turn still satisfies the head equation"),
  (``loop_exit_ok, "every one of a loop's delivered exits is typed at the state its `break` fires with"),
  (``class_unique, "§3's class assignment is unique; every derivation that reads a class off a type leans on this"),
  (``struct_carriesLinear_iff, "a struct's class carries `linear` iff a field's does — read off by the checker and by the destructure rules"),
  (``enum_carriesLinear_iff, "the same equation for an enum's variants"),
  -- eval/Step simulation
  (``init_safeAt, "`Config.init` is semantically safe; the fundamental lemma `step_preservation` inducts from"),
  (``eval_sim, "the simulation relation between `eval` and `Step`, proved for every expression, fuel and program"),
  (``eval_steps_of_outOfFuel, "exhausted fuel is a run of that many `Step`s — completeness modulo fuel, behind `eval_complete`"),
  (``step_value_typed, "every value a reachable `Step` configuration carries is typed"),
  (``destructure_plain, "a monitor removes no behaviour: the declared-linear destructure's residue check changes no step it does not refuse"),
  (``unwindLocs_plain, "a monitor removes no behaviour: an unwind's drops are the same with or without the monitors"),
  -- key trace lemmas
  (``eval_conserves, "the conservation law over `eval`'s identities, proved by fuel induction, that `no_double_free` follows from"),
  (``eval_tidy, "every cell an evaluation allocates is retired by its end — the frame-pop invariant behind `drop_exactly_once`"),
  (``rest_step, "the ledger for the rest of every form, behind `rest_exactly_once`"),
  (``run_blocks, "every finished run's trace is in the block grammar `Blocks`: each drop marker followed by exactly its own walk"),
  (``step_blocks, "carries `run_blocks` to `Step`"),
  (``reachable_ordered, "every scope record is in location order"),
  (``reachable_nested, "scopes nest, a pending `endscope` being the tail of its record"),
  (``reachable_lifo, "the registration stack is dropped newest-first"),
  (``pendingSafe_needed, "the RUE-2316 carve-out (a by-value argument a sibling's `return` destroys) is load-bearing, not vacuous"),
  (``roundRat_wf, "rounding an exact rational lands in 𝔽_w — the float model's closure law the non-vacuity witness rests on")
]

/-- (helper) The milestone names alone, without their reasons. -/
def milestoneNames : List Name := milestones.map (·.1)

/-- (helper) Every marked node the map draws: the spine (`Lint.headline`),
then the milestones, in that order and without duplicates. -/
def marked : List Name := Digest.dedup (Lint.headline ++ milestoneNames).toArray |>.toList

/-- (helper) Every milestone name that is not a theorem of the environment —
the generator's own check, since a stale or renamed entry in `milestones`
would otherwise silently draw no edges rather than fail loudly. -/
def milestoneProblems (env : Environment) : Array String :=
  milestones.foldl (init := #[]) fun acc (n, _) =>
    match Lint.find? env n with
    | some (.thmInfo _) => acc
    | _ => acc.push s!"{n}: listed in Map.milestones, but not a theorem of the environment"

/-! ## Walking a proof term for its marked ancestors -/

/-- (helper) The memo of the walk: every theorem, and every proof-bearing
`def` `followsInto` passes through, with the unmarked helper theorems its own
proof walks through before the next marked node (first), and the marked
nodes that walk meets (second) — both before any filtering, so a caller
reads whichever half it needs. In `CoreM` because `followsInto` asks the
environment (`isAutoDeclOrPrivate_Internal`) and elaborates a type
(`Meta.isProp`). -/
abbrev MarkM := ReaderT (Environment × NameSet) (StateT (NameMap (Array Name × Array Name)) CoreM)

/-- (helper) Should `walk` recurse into this constant's *value* the way it
does a theorem's — i.e. can this `def`'s body carry a further proof step
that a theorem's own `.value` does not mention directly, even though the
constant itself is never a marked node? Two cases, either sufficient on its
own:

* **compiler-generated** (`isAutoDeclOrPrivate_Internal`, the same predicate
  `Digest.isGenerated` starts from): a pattern-matched (`| pat => …`)
  theorem's equation-compiler auxiliary is a `def`, not a `theorem` — for
  example `Ctx.join_assoc`'s clause bodies live in `Ctx.join_assoc._f`, a
  `.defnInfo` the walk would otherwise treat as a dead end, silently losing
  every theorem that clause body calls;
* **Prop-valued** (`Meta.isProp` of the body under the type's own binders): a
  hand-written helper lemma that happens to be declared `def` rather than
  `theorem` is still a proof, whatever its keyword.

Neither test asks what a `def` *is about*, so an ordinary data or type
definition (`Ctx`, `OwnSt`, `check`, …) — which is Type- or data-valued, and
not compiler-generated merely for being pattern-matched itself — never
qualifies, and the walk does not wander from a theorem's proof into the
definition layer this way. A `def` is never itself a marked node, so
following one only ever adds edges and unmarked helper theorems already
reachable through it; it never changes what counts as marked. -/
def followsInto (info : ConstantInfo) (n : Name) : CoreM Bool := do
  if ← isAutoDeclOrPrivate_Internal n then return true
  Meta.MetaM.run' do
    Meta.forallTelescope info.type fun _ body => Meta.isProp body

/-- (helper) The value to walk for one constant: a theorem's proof, or —
only when `followsInto` says so — a proof-bearing `def`'s value; `none` for
everything else (a type, a structure, an ordinary computation, a
constructor, …). -/
def proofValue? (env : Environment) (n : Name) : CoreM (Option Lean.Expr) := do
  match Lint.find? env n with
  | some (.thmInfo v) => return some v.value
  | some ((.defnInfo v) : ConstantInfo) =>
      if ← followsInto (.defnInfo v) n then return some v.value else return none
  | _ => return none

/-- (helper) The unmarked helper theorems a marked or unmarked node's own
proof walks through — transparently, through any `followsInto` def in
between — and the marked nodes each path meets, stopping at the first marked
theorem it reaches rather than recursing into it — the shape of
`Lint.axiomsOf`'s memoized pass, for reachable marked theorems instead of
axioms. Every constant is walked once regardless of how many marked nodes
reach it: the result depends only on the constant and the marked set, not on
who is asking, so the top-level call on a marked node `B` itself (not a
recursive one) still walks `B`'s own proof — marking only applies to a
constant *met while walking*, never to the walk's own starting point. A
`followsInto` def passed through on the way is never itself added to the
helper count (it is plumbing, not a theorem); only the theorems reached
through it are. The walk never leaves the package (`Lint.inPackage`): a tactic
proof routinely mentions a Lean/Std theorem directly (`Eq.mpr`, a `List` or
`Nat` fact from `simp`), and following one of those into its own
equation-compiler auxiliaries — the same shape `followsInto` exists to see
past inside the package — would otherwise pull in an unrelated, effectively
unbounded slice of the standard library rather than RueCore's own proof
structure. -/
partial def walk (n : Name) : MarkM (Array Name × Array Name) := do
  if let some r := (← get).find? n then return r
  let (env, markedSet) ← read
  modify (·.insert n (#[], #[]))
  let mut helpers : Array Name := #[]
  let mut ancestors : Array Name := #[]
  match ← proofValue? env n with
  | none => pure ()
  | some v =>
      for c in v.getUsedConstants do
        -- stay inside the package: a proof that reaches a Lean/Std library lemma
        -- (say, a `List` or `Nat` fact) directly, or through one of its own
        -- pattern-matched auxiliaries, is not something RUE-2468's map is about,
        -- and following into the standard library's own `._f`/`.match_1`
        -- bridges would otherwise pull in an unrelated, unbounded amount of it
        if c == n || !Lint.inPackage env c then continue
        match Lint.find? env c with
        | some (.thmInfo _) =>
            if markedSet.contains c then
              ancestors := Lint.union ancestors #[c]
            else
              helpers := Lint.union helpers #[c]
              let (h2, a2) ← walk c
              helpers := Lint.union helpers h2
              ancestors := Lint.union ancestors a2
        | some ((.defnInfo v) : ConstantInfo) =>
            if ← followsInto (.defnInfo v) c then
              let (h2, a2) ← walk c
              helpers := Lint.union helpers h2
              ancestors := Lint.union ancestors a2
        | _ => pure ()
  let r := (helpers, ancestors)
  modify (·.insert n r)
  return r

/-- (helper) `walk` run over every marked node, threading one memo through
all of them: the marked ancestors reached from each (for the spine diagram's
edges and a spine theorem's milestone ancestors), and the distinct unmarked
helper theorem count under each (the size stats). -/
def walkAll (env : Environment) (markedList : List Name) :
    CoreM (NameMap (Array Name) × NameMap Nat) := do
  let markedSet := markedList.foldl (init := NameSet.empty) (·.insert ·)
  let mut memo : NameMap (Array Name × Array Name) := {}
  let mut ancestorsOf : NameMap (Array Name) := {}
  let mut helperCountOf : NameMap Nat := {}
  for n in markedList do
    let ((helpers, ancestors), memo') ← ((walk n).run (env, markedSet)).run memo
    memo := memo'
    ancestorsOf := ancestorsOf.insert n ancestors
    helperCountOf := helperCountOf.insert n helpers.size
  return (ancestorsOf, helperCountOf)

/-! ## Reading a doc-comment for its calculus citations -/

/-- (helper) A string, without a duplicate, in first-occurrence order. -/
def dedupStr (xs : Array String) : Array String :=
  xs.foldl (init := (#[] : Array String)) fun acc x => if acc.contains x then acc else acc.push x

/-- (helper) The `§N.M` (and `§N.M–§N.M'`) section citations a doc-comment
contains, as written, in the doc-comment convention's first spelling
(`../../README.md`, "Doc-comment convention"). Informational only: unlike the
index generator (`scripts/validate-lean-xref-index.py`), this does not check
the label against the calculus, so it is read as a pointer for a reviewer,
not as a citation gate. -/
partial def sectionCitations (doc : String) : Array String := Id.run do
  let cs := doc.toList.toArray
  let mut out : Array String := #[]
  let mut i := 0
  while i < cs.size do
    if cs[i]! == '§' then
      let mut j := i + 1
      let mut text : Array Char := #['§']
      -- a `.` only continues the citation when a digit follows, so a sentence's
      -- full stop right after a section number (`§6.8.`) is not swallowed
      while j < cs.size && (cs[j]!.isDigit ||
          (cs[j]! == '.' && (cs[j+1]?.map Char.isDigit).getD false)) do
        text := text.push cs[j]!
        j := j + 1
      if text.size > 1 then out := out.push (String.ofList text.toList)
      i := j
    else i := i + 1
  return dedupStr out

/-- (helper) Can this character continue a parenthesized rule label
(`Use-Move`, `D-Let`, `@Drop-Copy`)? -/
def isRuleChar (c : Char) : Bool := c.isAlpha || c.isDigit || c == '-' || c == '@'

/-- (helper) The parenthesized rule-label citations a doc-comment contains: a
parenthesized span of `isRuleChar` text starting with `@` or an upper-case
letter, read the same way the doc-comment convention's second spelling does,
less an issue id (`RUE-2469`), which the same shape of parentheses could
otherwise be mistaken for. Informational, like `sectionCitations`: it does
not check the label against the calculus. -/
partial def ruleCitations (doc : String) : Array String := Id.run do
  let cs := doc.toList.toArray
  let mut out : Array String := #[]
  let mut i := 0
  while i < cs.size do
    if cs[i]! == '(' then
      let mut j := i + 1
      let mut text : Array Char := #[]
      while j < cs.size && cs[j]! != ')' && isRuleChar cs[j]! do
        text := text.push cs[j]!
        j := j + 1
      if j < cs.size && cs[j]! == ')' && !text.isEmpty then
        let s := String.ofList text.toList
        let starts := text[0]!
        if (starts == '@' || starts.isUpper) && !s.startsWith "RUE-" then
          out := out.push ("(" ++ s ++ ")")
      i := j + 1
    else i := i + 1
  return dedupStr out

/-! ## Rendering -/

/-- (helper) A declaration's name, sanitized into a Mermaid-safe node or
subgraph id: every character but a letter or a digit becomes `_`, so a dotted
name (`Ctx.join_absorb`) or a module name (`Statics.Lemmas`) is one token
Mermaid never misreads. -/
def sanitizeId (s : String) : String :=
  String.ofList (s.toList.map fun c => if c.isAlphanum then c else '_')

/-- (helper) A declaration's proof size in source lines, from the range the
compiled module recorded for it (`Lean.declRangeExt`), or `0` when it has
none. -/
def declLines (name : Name) : CoreM Nat := do
  match ← findDeclarationRanges? name with
  | some ranges => return ranges.range.endPos.line - ranges.range.pos.line + 1
  | none => return 0

/-- (helper) The static assurance-chain diagram (RUE-2468): what the proof
chain covers, and how the bridge corpus tests the compiler against the same
model, kept beside `../WHAT-IT-MEANS.md`'s diagram (RUE-2462) in content
without depending on that file, since it lands on a separate branch. Static
because the chain from the calculus to the compiler's targets is not a
question the compiled environment answers; the spine and per-theorem diagrams
above it are. -/
def assuranceChainDiagram : List String := [
  "```mermaid",
  "flowchart LR",
  "    calc[\"Core calculus<br/>(01-core-calculus.md)\"] --> defs[\"Lean definitions<br/>(L0 syntax, L1 definitions)\"]",
  "    defs --> spec[\"Spec statements<br/>(RueCore.Spec.spine)\"]",
  "    spec --> proofs[\"L2 proofs<br/>(the spine and the milestones)\"]",
  "    proofs --> kernel[\"Lean kernel<br/>(propext, Quot.sound only)\"]",
  "    proofs --> comparator[\"Lean Comparator<br/>(independent replay)\"]",
  "    proofs --> lint[\"ruecore-lint<br/>(trusted base, axiom allow-list)\"]",
  "    defs --> interp[\"Interpreter<br/>(eval)\"]",
  "    defs --> print[\"Printer:<br/>core program to Rue source\"]",
  "    print --> rue[\"Rue programs<br/>(hand-written + generated)\"]",
  "    rue --> comp[\"Compiler:<br/>accept or reject\"]",
  "    comp --> oracle[\"Compiler's reference<br/>interpreter\"]",
  "    comp --> native[\"Native code<br/>-O1 / -O2 / -O3\"]",
  "    interp -- \"expected answer\" --> cmp{\"Compare\"}",
  "    comp --> cmp",
  "    oracle --> cmp",
  "    native --> cmp",
  "    cmp -- \"disagreement\" --> issue[\"Issue: compiler bug,<br/>model bug or spec question\"]",
  "```"]

end RueCore.Map
