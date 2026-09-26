#!/usr/bin/env python3
"""Mutation analysis of the mechanization's definitions (RUE-2465; results in ../MUTATION.md).

usage: mutate.py --work DIR [--src LEANDIR] [--only ID,...] [--redo] [--compiler-root WT]
                 [--gen N] [--gen-seed S] [--lake-cache DIR]
       mutate.py --list | --check | --work DIR --table

Each mutant is a small, deliberate change to the semantics and the checker: the L0 module
`Syntax` and five of L1's seven modules (`Statics`, `Checker/Defs`, `Dynamics`, `Step`; README,
"Layers"), written as exact-text edits of the package's sources. L1's statement vocabulary
(`Soundness/Defs`, `Trace/Defs`, `Adequacy/Defs`) and L0's `Float` are not mutated (RUE-2490).
For each mutant the script copies the package's pristine sources into a scratch copy under
--work (never the package itself), applies the edits, and records the first of these that
notices (kills) it:

  proof    `lake build` fails in a theorem of layers L0-L2 or the Spec layer (in practice an
           L2 module: `*/Lemmas`, `Soundness`, `Checker`, `Trace`, `Adequacy`, `TraceExact`,
           `TraceOrder`, `Spine`);
  witness  `lake build` fails only in L3 (`Examples`, `Witnesses`, `Corpus`, `Print`,
           `Explain`, ...) or in an `example`;
  mirror   a witness failure that is only in `Explain.lean`, whose checker and interpreter
           are second copies of `check` and `eval`: a mismatch between two copies of a
           definition, not a test of what the definition means;
  corpus   the build succeeds but `lake exe ruecore-corpus` (the seed corpus: verdicts and
           expected outcomes) differs from the unmutated baseline;
  bridge   the seeds are unchanged, but the generated corpus (`--gen N --seed S`, 200 at
           seed 7 by default, the per-lane check's) differs from the baseline on a case where
           the compiler (`<compiler-root>/scripts/rue exec`) disagrees with the mutant.
           A generated case the mutant does not change cannot disagree anew, so only the
           changed cases are run; with no --compiler-root, a changed case is reported as
           `gen-changed` and not run;
  survived nothing above notices.

The module lists come from `RueCore/Layers.lean`'s `table`, the list the layering audit checks
(`configure`): the mutants may edit layers 0-1, the proofs-off copy turns off every theorem of
layers 0-3 (L0, L1, Spec, L2), and layer 4 (L3) holds the witnesses.

A seed the mutant leaves unchanged cannot make the bridge disagree anew (its expectation is
the baseline's, which the bridge already compares), so the bridge column is the generated
cases alone. The kill order is the order above and the first killer is the mutant's result.

**The proofs-off pass.** A proof can break for a reason that is not semantic: a proof that
destructures a rule's premises by position breaks when a premise is deleted, whether or not the
statement it proves is still true. And the corpus imports the proofs, so a proof kill hides
what the rest of the suite would say. So every mutant a proof kills is run a second time in a
second scratch package in which every theorem of layers 0-3 (`PROOF_FILES`) has its proof
replaced by `sorry` (its statement kept), recording the first of witness / mirror / corpus /
bridge / survived, or `definition` when a layer 0-1 definition no longer elaborates. A failure
in a layer 2-3 proof there means the proofs-off copy is wrong, and the run stops with a script
error rather than record it. The proofs-off baseline must reproduce the baseline's corpus
exactly. A changed case the mutant's checker accepts and its machine refuses is listed as
`unsound`: a concrete counterexample to `check_sound` plus soundness, whatever the proof
scripts say.

**The corpus-only pass.** Likewise a witness or mirror kill (at either level) hides what the
corpus and the bridge alone would say, because `Corpus.lean` imports `Examples.lean`. Such a
mutant is run a third time, in a third package that also turns off the witnesses: every
`theorem` and `example` of layer 4, and every `example` of layers 0-3, gets `sorry`, and each
`#guard` is commented out. That pass records corpus / bridge / survived (or `definition` if a
definition still fails to elaborate). Each pass's baseline must reproduce the full baseline's
corpus.

The results are written to <work>/results.json (one entry per mutant, merged across runs, so
an interrupted run resumes where it stopped). `--table` prints them as MUTATION.md's table,
with the reading of each proof kill (`RULINGS`). The scratch packages' `.lake` directories are
reused between mutants; --lake-cache seeds them from an existing build (a warm start). One
build runs at a time.

The mutants are `MUTANTS` below; `--list` prints them. `--check` checks that every module of
the package is in Layers.lean's table (and every entry is a module), that every mutant's edits
apply to the current sources exactly as many times as they say and touch only layers 0-1, and
that the proofs-off and witnesses-off copies turn off every theorem, example and `#guard`.

Reproduce from a clean checkout:
  python3 docs/formal/lean/bin/mutate.py --work /tmp/rue-mut --compiler-root $PWD
"""
import argparse, json, os, re, shutil, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_SRC = os.path.dirname(HERE)

# The package's modules by layer, read from `RueCore/Layers.lean`'s `table` (the one list the
# layering audit checks), so the lists below cannot drift from the package. The table's layer
# numbers: 0 = L0 syntax, 1 = L1 definitions, 2 = the Spec statements, 3 = L2 proofs,
# 4 = L3 tooling (README, "Layers"). `configure` fills them in from --src.
LAYER_ENTRY = re.compile(r"\(`(RueCore(?:\.[\w.]+)?),\s*(\d+)\)")
DEFINITION_LAYERS = (0, 1)      # what the mutants may edit
PROOF_LAYERS = (0, 1, 2, 3)     # whose theorems the proofs-off pass turns off
WITNESS_LAYERS = (4,)           # examples, witnesses, the corpus, the printer, Explain
LAYER_OF = {}                   # "RueCore/X/Y.lean" -> layer
DEFINITION_FILES = ()
PROOF_FILES = ()
WITNESS_FILES = ()
# Explain's executable mirrors of the checker and the interpreter: a failure there is a
# mismatch between two copies of a definition, not a test of the definition's meaning.
MIRROR_FILES = ("RueCore/Explain.lean",)


def module_path(name):
    return name.replace(".", "/") + ".lean"


def read_layers(src):
    t = open(os.path.join(src, "RueCore/Layers.lean"), encoding="utf-8").read()
    i = t.index("def table")
    j = t.index("]", i)
    return {module_path(n): int(l) for n, l in LAYER_ENTRY.findall(t[i:j])}


def configure(src):
    global LAYER_OF, DEFINITION_FILES, PROOF_FILES, WITNESS_FILES
    LAYER_OF = read_layers(src)
    DEFINITION_FILES = tuple(sorted(f for f, l in LAYER_OF.items() if l in DEFINITION_LAYERS))
    PROOF_FILES = tuple(sorted(f for f, l in LAYER_OF.items() if l in PROOF_LAYERS))
    WITNESS_FILES = tuple(sorted(f for f, l in LAYER_OF.items() if l in WITNESS_LAYERS))


def E(file, old, new, count=1):
    return {"file": file, "old": old, "new": new, "count": count}


def M(id, sect, rule, op, edits, note=""):
    return {"id": id, "sect": sect, "rule": rule, "op": op, "edits": edits, "note": note}


ST, CK, DY, SP, SX = ("RueCore/Statics.lean", "RueCore/Checker/Defs.lean",
                      "RueCore/Dynamics.lean", "RueCore/Step.lean", "RueCore/Syntax.lean")

# Operators (the issue's, plus two of the classic ones): premise = drop a premise (in a
# `Typed` rule the premise is replaced by `True`, which is the same rule but keeps the
# constructor's arity, so a proof that only destructures the rule by position still checks);
# join = swap/alter the §5.5 join; move-copy = make a move a copy; drop-skip / drop-order =
# skip or reorder a drop; copy-check = weaken a Copy check; affine-linear = Affine<->Linear;
# bounds = remove a bounds trap; order = change evaluation order; off-by-one; trap = alter a
# trap; monitor = remove a run-time monitor; operand = swap or alter an operator's arithmetic;
# completeness = make the checker reject more; equivalent-candidate = a change believed harmless.
MUTANTS = [
    # ---------------- §5.1 uses ----------------
    M("use-move-partial", "§5.1", "(Use-Move)", "premise",
      [E(ST, "      en.st.get p.path = some u → u.fullyOwned = true →\n      en.ty.atPath P.decls p.path = some T →\n      T.mult P.decls ≠ .copy →\n      noDtorPrefix",
         "      en.st.get p.path = some u → True →\n      en.ty.atPath P.decls p.path = some T →\n      T.mult P.decls ≠ .copy →\n      noDtorPrefix"),
       E(CK, "(if u.fullyOwned ∧ noDtorPrefix P.decls en.ty p.path ∧\n                       rootIdxOnly",
         "(if noDtorPrefix P.decls en.ty p.path ∧\n                       rootIdxOnly")],
      "fully-owned dropped: a partially moved aggregate may be moved whole"),
    M("use-copy-moved", "§5.1", "(Use-Copy)", "premise",
      [E(ST, "      en.st.get p.path = some u → u.fullyOwned = true →\n      en.ty.atPath P.decls p.path = some T →\n      T.mult P.decls = .copy →\n      declaredPrefix P.decls en.ty p.path = none →\n      Typed P R Γ (.use p)",
         "      en.st.get p.path = some u → True →\n      en.ty.atPath P.decls p.path = some T →\n      T.mult P.decls = .copy →\n      declaredPrefix P.decls en.ty p.path = none →\n      Typed P R Γ (.use p)"),
       E(CK, "(if u.fullyOwned then some (.ty T, ⟨some Γ, []⟩) else none)",
         "(some (.ty T, ⟨some Γ, []⟩))")],
      "fully-owned dropped from the Copy use"),
    M("use-move-dtor", "§5.1", "(Use-Move) 3.9:34", "premise",
      [E(ST, "      T.mult P.decls ≠ .copy →\n      noDtorPrefix P.decls en.ty p.path = true →\n      declaredPrefix P.decls en.ty p.path = none →\n      rootIdxOnly",
         "      T.mult P.decls ≠ .copy →\n      True →\n      declaredPrefix P.decls en.ty p.path = none →\n      rootIdxOnly"),
       E(CK, "(if u.fullyOwned ∧ noDtorPrefix P.decls en.ty p.path ∧\n                       rootIdxOnly",
         "(if u.fullyOwned ∧\n                       rootIdxOnly")],
      "E0456 dropped: a field may be moved out of a destructor-bearing value"),
    M("use-move-rootidx", "§5.1", "(Use-Move) 3.8:68", "premise",
      [E(ST, "      declaredPrefix P.decls en.ty p.path = none →\n      rootIdxOnly P.decls en.ty p.path = true →\n      Typed P R Γ (.use p)",
         "      declaredPrefix P.decls en.ty p.path = none →\n      True →\n      Typed P R Γ (.use p)"),
       E(CK, "u.fullyOwned ∧ noDtorPrefix P.decls en.ty p.path ∧\n                       rootIdxOnly P.decls en.ty p.path then\n                      some (.ty T,",
         "u.fullyOwned ∧ noDtorPrefix P.decls en.ty p.path then\n                      some (.ty T,")],
      "an element may be moved out below a projection (a[i] only at the root)"),
    M("use-affine-as-copy", "§5.1", "(Use-Copy)/(Use-Move)", "move-copy",
      [E(ST, "      T.mult P.decls = .copy →\n      declaredPrefix P.decls en.ty p.path = none →\n      Typed P R Γ (.use p) T ⟨some Γ, []⟩",
         "      T.mult P.decls ≠ .linear →\n      declaredPrefix P.decls en.ty p.path = none →\n      Typed P R Γ (.use p) T ⟨some Γ, []⟩"),
       E(CK, "                 if T.mult P.decls = .copy then\n                   (if u.fullyOwned then some (.ty T,",
         "                 if T.mult P.decls ≠ .linear then\n                   (if u.fullyOwned then some (.ty T,")],
      "an affine use leaves the place Owned (statics copies what the machine moves)"),
    M("use-declared-residue", "§5.1", "(Use-Declared-Linear-Destructure)", "premise",
      [E(SX, "               anyLinearOther D sd.fields f ||\n", "")],
      "linearResidue ignores a linear sibling field of the destructured leaf"),
    M("index-read-copy", "§5.1", "(Use-Untrackable-Dynamic-Copy)", "copy-check",
      [E(ST, "      Ta.atDyn P.decls πs = some T →\n      T.mult P.decls = .copy →\n      declaredPrefix P.decls en.ty p.path = none →\n      Ta.dynNoDeclared P.decls πs = true →\n      Typed P R Γ (.indexRead",
         "      Ta.atDyn P.decls πs = some T →\n      True →\n      declaredPrefix P.decls en.ty p.path = none →\n      Ta.dynNoDeclared P.decls πs = true →\n      Typed P R Γ (.indexRead"),
       E(CK, "u.fullyOwned ∧\n                      T.mult P.decls = .copy ∧\n                      declaredPrefix P.decls en.ty p.path = none ∧\n                      Ta.dynNoDeclared P.decls πs then some (.ty T, ⟨some Γ₁, Δ⟩)",
         "u.fullyOwned ∧\n                      declaredPrefix P.decls en.ty p.path = none ∧\n                      Ta.dynNoDeclared P.decls πs then some (.ty T, ⟨some Γ₁, Δ⟩)")],
      "a dynamic-index read of a non-Copy element is admitted"),
    M("index-drop-copy-checker", "§5.1", "(Use-Untrackable-Dynamic-Copy), @drop", "copy-check",
      [E(CK, "u.fullyOwned ∧\n                      T.mult P.decls = .copy ∧\n                      declaredPrefix P.decls en.ty p.path = none ∧\n                      Ta.dynNoDeclared P.decls πs then some (.ty .unit, ⟨some Γ₁, Δ⟩)",
         "u.fullyOwned ∧\n                      declaredPrefix P.decls en.ty p.path = none ∧\n                      Ta.dynNoDeclared P.decls πs then some (.ty .unit, ⟨some Γ₁, Δ⟩)")],
      "checker only: `@drop(a[i])` of a non-Copy element accepted; the rule is unchanged"),
    M("const-index-off-by-one", "§5.1", "Ty.atPath (7.1:9)", "off-by-one",
      [E(SX, "  | .array T n, c => if c < n then some T else none",
         "  | .array T n, c => if c ≤ n then some T else none")],
      "a constant index equal to the length types"),
    # ---------------- §5.2 assignment ----------------
    M("assign-overwrite", "§5.2", "(Assign) 3.8:77", "premise",
      [E(ST, "      (u₁ = .movedOut ∨ T.mult P.decls ≠ .linear) →\n", "      True →\n"),
       E(CK, "if assignArrayOk P.decls en₁.st en₁.ty p.path ∧\n                             overwriteOk P.decls u₁ T then",
         "if assignArrayOk P.decls en₁.st en₁.ty p.path then")],
      "a live linear place may be overwritten (E0493 dropped)"),
    M("assign-array-ok", "§5.2", "(Assign) 3.8:72", "premise",
      [E(ST, "      assignArrayOk P.decls en₁.st en₁.ty p.path = true →\n      (u₁ = .movedOut",
         "      True →\n      (u₁ = .movedOut"),
       E(CK, "if assignArrayOk P.decls en₁.st en₁.ty p.path ∧\n                             overwriteOk",
         "if overwriteOk")],
      "E0480 dropped: a write into an array with a moved-out element"),
    M("assign-immutable", "§5.2", "(Assign) mut", "premise",
      [E(ST, "      Γ[p.root]? = some en₀ → en₀.mu = true →\n      en₀.st.get p.path = some u₀ →\n      en₀.ty.atPath P.decls p.path = some T →",
         "      Γ[p.root]? = some en₀ → True →\n      en₀.st.get p.path = some u₀ →\n      en₀.ty.atPath P.decls p.path = some T →"),
       E(CK, "        if en₀.mu = true then\n          match en₀.st.get p.path, en₀.ty.atPath P.decls p.path with\n          | some _, some T =>",
         "        if True then\n          match en₀.st.get p.path, en₀.ty.atPath P.decls p.path with\n          | some _, some T =>")],
      "assignment to an immutable binding admitted"),
    M("index-write-linear", "§5.2", "(Assign) at a dynamic index", "premise",
      [E(ST, "      assignArrayOk P.decls en₁.st en₁.ty p.path = true →\n      T.mult P.decls ≠ .linear →\n", "      assignArrayOk P.decls en₁.st en₁.ty p.path = true →\n      True →\n"),
       E(CK, "assignArrayOk P.decls en₁.st en₁.ty p.path ∧\n                                   T.mult P.decls ≠ .linear then",
         "assignArrayOk P.decls en₁.st en₁.ty p.path then")],
      "a linear element may be written through a dynamic index"),
    # ---------------- §5.3 drop, seq ----------------
    M("drop-residual-below", "§5.3", "(@Drop) E0406", "premise",
      [E(ST, "      (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) →\n", "      True →\n"),
       E(CK, "                       (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) ∧\n", "")],
      "@drop of a partially moved value with a live linear sub-place admitted"),
    M("drop-moved", "§5.3", "(@Drop)", "premise",
      [E(ST, "      en.st.get p.path = some u → u.isOwned = true →\n", "      en.st.get p.path = some u → True →\n"),
       E(CK, "(if u.isOwned ∧ noDtorPrefix", "(if noDtorPrefix")],
      "@drop of a moved-out place admitted"),
    M("seq-discard", "§5.3", "(Seq) 3.8:64", "premise",
      [E(ST, "      Typed P R Γ e₁ T₁ ⟨some Γ₁, Δ₁⟩ → T₁.mult P.decls ≠ .linear →\n", "      Typed P R Γ e₁ T₁ ⟨some Γ₁, Δ₁⟩ → True →\n"),
       E(CK, "          if T₁.mult P.decls = .linear then none\n          else", "          if False then none\n          else")],
      "a linear value may be discarded by `;`"),
    # ---------------- §5.5 branches and join ----------------
    M("join-owned-wins", "§5.5", "join", "join",
      [E(ST, "  | .owned, b, T => if ownedJoinOk D b T then some b else none\n  | a, .owned, T => if ownedJoinOk D a T then some a else none",
         "  | .owned, b, T => if ownedJoinOk D b T then some .owned else none\n  | a, .owned, T => if ownedJoinOk D a T then some .owned else none")],
      "join(Owned, t) = Owned: the less-moved state wins"),
    M("join-linear-disagree", "§5.5", "join 3.8:50 (E0443)", "join",
      [E(ST, "  | .movedOut, T => decide (T.mult D ≠ .linear)\n  | .fields ts, .struct s =>\n      (match D.structs[s]? with\n       | some sd => ownedJoinOkList",
         "  | .movedOut, _ => true\n  | .fields ts, .struct s =>\n      (match D.structs[s]? with\n       | some sd => ownedJoinOkList")],
      "a linear path Owned on one arm and MovedOut on the other joins"),
    M("join-residual", "§5.5", "join (residual reading)", "join",
      [E(ST, "  | .movedOut, b, T => if residualLinear D b T then none else some .movedOut\n  | a, .movedOut, T => if residualLinear D a T then none else some .movedOut",
         "  | .movedOut, _, _ => some .movedOut\n  | _, .movedOut, _ => some .movedOut")],
      "MovedOut against a partially moved node joins without the residual check"),
    M("join-diverge-arm", "§5.5/§5.7", "join over Ω (Sub-Never)", "join",
      [E(ST, "  | none, o => some o\n  | some a, none => some (some a)",
         "  | none, _ => some none\n  | some a, none => some (some a)")],
      "a diverging arm makes the whole branch diverge"),
    M("meet-never", "§5.5/§5.7", "(If) arm type, (Sub-Never)", "completeness",
      [E(CK, "  | .never, c => some c\n  | c, .never => some c", "  | .never, _ => none\n  | _, .never => none")],
      "checker only: a diverging arm no longer meets a typed one"),
    M("first-arm-ty", "§5.5", "(Match) arm type", "completeness",
      [E(CK, "      | some (.ty T, _) => .ty T\n      | _ => firstArmTy P R Γ₀ es Tss",
         "      | some (.ty T, _) => .ty T\n      | _ => .never")],
      "checker only: the arms' type is the first arm's even when it diverges"),
    M("match-exhaustive", "§5.5", "(Match) exhaustiveness", "equivalent-candidate",
      [E(ST, "      arms.length = ed.variants.length →\n      TypedArms", "      True →\n      TypedArms"),
       E(CK, "           if arms.length = ed.variants.length then", "           if True then")],
      "the arm-count premise dropped; TypedArms/checkArms still align arms with variants"),
    M("arm-leak", "§5.5/§5.6", "(Match) arm scope exit", "premise",
      [E(ST, "      NoResidualLinear P.decls (Γb.take Ts.length) →\n", "      True →\n"),
       E(CK, "if c'.fitsC c ∧ NoResidualLinear P.decls (Γb.take Ts.length) then", "if c'.fitsC c then")],
      "a match arm may end with a live linear payload binding"),
    M("arm-payload-mutable", "§5.5", "(Match) payload binders", "premise",
      [E(ST, "  (Ts.map fun T => ({ ty := T, mu := false, st := .owned } : Entry)).reverse ++ Γ",
         "  (Ts.map fun T => ({ ty := T, mu := true, st := .owned } : Entry)).reverse ++ Γ")],
      "match payload bindings are mutable"),
    # ---------------- §5.6 scope exit ----------------
    M("let-leak", "§5.6", "(Let) scope exit", "premise",
      [E(ST, "      residualLinear P.decls en'.st en'.ty = false →\n      Typed P R Γ (.letIn", "      True →\n      Typed P R Γ (.letIn"),
       E(CK, "             if residualLinear P.decls en'.st en'.ty then none\n             else some",
         "             if False then none\n             else some")],
      "a let may end with a live linear binding"),
    M("residual-declared", "§5.6", "residual-linear (3.8:74)", "affine-linear",
      [E(ST, "       | some sd => sd.attr = .linear || residualLinearFields D ts sd.fields",
         "       | some sd => residualLinearFields D ts sd.fields")],
      "a partially moved declared-linear struct carries no obligation of its own"),
    M("residual-untracked", "§5.6", "residual-linear, untracked residue", "affine-linear",
      [E(ST, "  | [], Ts => Ts.any fun T => decide (T.mult D = .linear)\n  | _ :: _, [] => false\n  | t :: ts, T :: Ts => residualLinear D t T || residualLinearFields D ts Ts",
         "  | [], _ => false\n  | _ :: _, [] => false\n  | t :: ts, T :: Ts => residualLinear D t T || residualLinearFields D ts Ts")],
      "§5.6's second disjunct dropped: untouched slots carry nothing"),
    # ---------------- §5.7 control ----------------
    M("return-leak", "§5.7", "(Return-Value)", "premise",
      [E(ST, "      NoResidualLinear P.decls Γ₁ →\n      Typed P R Γ (.ret e)", "      True →\n      Typed P R Γ (.ret e)"),
       E(CK, "if c.fits R ∧ NoResidualLinear P.decls Γ₁ then", "if c.fits R then")],
      "`return` may leave a live linear binding"),
    M("break-leak", "§5.7", "(Loop-Break) loop locals", "premise",
      [E(ST, "      (∀ Γb ∈ Ωe.brk, NoResidualLinear P.decls (Ctx.loopLocals Γh Γb)) →\n", "      True →\n"),
       E(CK, "                  if (Γb₀ :: Γbs).all\n                      (fun Γb => decide (NoResidualLinear P.decls (Ctx.loopLocals Γh Γb))) then",
         "                  if true then")],
      "`break` may leave a live linear loop-local"),
    M("loop-div-breaks", "§5.7", "(Loop-Div)", "premise",
      [E(ST, "      LoopHead P.decls Γ Ωe.norm Γh →\n      e.breaks = false →\n", "      LoopHead P.decls Γ Ωe.norm Γh →\n      True →\n")],
      "rules only: a loop whose body breaks may be typed as diverging"),
    M("loop-break-div-brk", "§5.7", "(Loop-Break), no reachable exit", "premise",
      [E(ST, "      e.breaks = true →\n      Ωe.brk = [] →\n", "      e.breaks = true →\n      True →\n")],
      "rules only: a breaking loop whose breaks are reachable may be typed ⊥"),
    M("loop-head-unverified", "§5.7", "loop head (LoopHead)", "premise",
      [E(CK, "if c.fits .unit ∧ Ctx.joinOpt P.decls (some Γ) Ωe.norm = some (some Γh) ∧",
         "if c.fits .unit ∧")],
      "checker only: the iterated head is not re-verified against the equation"),
    M("head-iter-bound", "§5.7", "loop head iteration", "completeness",
      [E(CK, "          (e.nodes + 2) Γ with", "          1 Γ with")],
      "checker only: one head step, so a loop whose body moves is refused"),
    M("breaks-nested", "§5.7", "Expr.breaks", "premise",
      [E(SX, "  | .brk => true\n  | .loop _ => false", "  | .brk => true\n  | .loop e => e.breaks")],
      "an inner loop's break counts as the outer loop's"),
    # ---------------- §5.8 functions, intro, operators, §3 classes ----------------
    M("fn-exit-leak", "§5.8", "(Fn) exit edge", "premise",
      [E(ST, "    (∀ Γf, Ωf.norm = some Γf → NoResidualLinear P.decls Γf) ∧ Ωf.brk = []", "    True ∧ Ωf.brk = []"),
       E(CK, "        (match Ω.norm with\n         | some Γf => decide (NoResidualLinear P.decls Γf)\n         | none => true) &&\n", "")],
      "a function body may end with a live linear parameter or local"),
    M("fn-params-order", "§5.8", "(Fn) entry context", "order",
      [E(ST, "  (fd.params.map fun p => { ty := p.ty, mu := p.mu, st := .owned }).reverse",
         "  (fd.params.map fun p => { ty := p.ty, mu := p.mu, st := .owned })")],
      "parameters bound in the wrong de Bruijn order"),
    M("entry-params", "§6.12", "top-level main()", "premise",
      [E(CK, "     | some fd => fd.params.isEmpty", "     | some _ => true")],
      "checker only: an entry function with parameters accepted"),
    M("lit-bounds", "§5.8", "(Lit)", "premise",
      [E(ST, "      InBounds w s n →\n      Typed P R Γ (.intLit w s n)", "      True →\n      Typed P R Γ (.intLit w s n)"),
       E(CK, "  | .intLit w s n => if InBounds w s n then some (.ty (.int w s), ⟨some Γ, []⟩) else none",
         "  | .intLit w s n => some (.ty (.int w s), ⟨some Γ, []⟩)")],
      "an out-of-range integer literal types"),
    M("dbg-observable", "§5.8", "(Dbg)", "premise",
      [E(ST, "      Typed P R Γ e T Ω → T.observable = true →\n      Typed P R Γ (.dbg e)", "      Typed P R Γ e T Ω → True →\n      Typed P R Γ (.dbg e)"),
       E(CK, "if T.observable then some (.ty .unit, Ω) else none", "some (.ty .unit, Ω)")],
      "@dbg of an aggregate types"),
    M("repeat-copy", "§5.8", "array repeat (7.1:36)", "copy-check",
      [E(ST, "      Typed P R Γ e T Ω → T.mult P.decls = .copy →\n      Typed P R Γ (.repeatArray", "      Typed P R Γ e T Ω → True →\n      Typed P R Γ (.repeatArray"),
       E(CK, "if T' = T ∧ T.mult P.decls = .copy then", "if T' = T then")],
      "`[e; n]` of a non-Copy element types"),
    M("class-not-infectious", "§3", "class of a struct (Attr.lift)", "affine-linear",
      [E(SX, "  | .none, base => if base = .linear then .linear else .affine", "  | .none, _ => .affine")],
      "a plain struct with a linear field is Affine"),
    M("mult-join-meet", "§3", "class join", "affine-linear",
      [E(SX, "def Mult.join (a b : Mult) : Mult := if a.rank ≤ b.rank then b else a",
         "def Mult.join (a b : Mult) : Mult := if a.rank ≤ b.rank then a else b")],
      "the class join takes the lesser class"),
    M("zero-array-linear", "§3", "class of [T; 0] (3.8:74)", "affine-linear",
      [E(SX, "      | m => if n = 0 then .affine else m", "      | m => m")],
      "a zero-length array of a linear type is Linear"),
    M("copy-struct-dtor", "§3", "@copy struct (3.9:31)", "premise",
      [E(CK, "     | .copy => decide (sd.baseOf D = .copy) && !sd.dtor", "     | .copy => decide (sd.baseOf D = .copy)")],
      "checker only: a @copy struct with a destructor accepted"),
    M("dtor-linear-field", "§3", "destructor with a linear field (3.9:44)", "premise",
      [E(CK, "    (!sd.dtor || !decide (sd.baseOf D = .linear))", "    true")],
      "checker only: a destructor-bearing struct may hold a linear field"),
    M("decl-cycle-rounds", "§3", "acyclicity 3.0:5 (E0483)", "completeness",
      [E(CK, "  let st := D.peel (D.structs.length + D.enums.length)", "  let st := D.peel D.structs.length")],
      "checker only: too few peel rounds, so a deep acyclic nesting is refused"),
    M("entry-join-bty", "§5.5", "Entry.join", "equivalent-candidate",
      [E(ST, "  (OwnSt.join D a.st b.st a.ty).map a.setSt", "  (OwnSt.join D a.st b.st b.ty).map a.setSt")],
      "join at the second entry's type (REDTEAM-LOG: 'ignores the second entry's type')"),
    # ---------------- §6 dynamics ----------------
    M("dyn-move-as-copy", "§6.3", "(D-Use-Move)", "move-copy",
      [E(DY, "                  if v.mult P.decls = .copy then .ok H v []", "                  if v.mult P.decls ≠ .linear then .ok H v []"),
       E(SP, "            if v.mult P.decls = .copy then .next (.run H φ K (.ret v) tr)", "            if v.mult P.decls ≠ .linear then .next (.run H φ K (.ret v) tr)"),
       E(SP, "      v.mult P.decls = .copy →\n      Step M P (.run H φ K (.eval (.use p)) tr) (.run H φ K (.ret v) tr)",
         "      v.mult P.decls ≠ .linear →\n      Step M P (.run H φ K (.eval (.use p)) tr) (.run H φ K (.ret v) tr)"),
       E(SP, "      v.mult P.decls ≠ .copy →\n      c.writeAt p.path .hole = some c' →\n      Step M P (.run H φ K (.eval (.use p)) tr)",
         "      v.mult P.decls = .linear →\n      c.writeAt p.path .hole = some c' →\n      Step M P (.run H φ K (.eval (.use p)) tr)")],
      "eval, Step and step: an affine use copies instead of leaving a hole"),
    M("step-usecopy-nondet", "§6.3", "(D-Use-Copy), Step only", "copy-check",
      [E(SP, "      v.mult P.decls = .copy →\n      Step M P (.run H φ K (.eval (.use p)) tr) (.run H φ K (.ret v) tr)",
         "      Step M P (.run H φ K (.eval (.use p)) tr) (.run H φ K (.ret v) tr)")],
      "Step only: the copy rule's Copy premise dropped"),
    M("bounds-off-by-one", "§6.5", "(D-Index-Trap)", "off-by-one",
      [E(DY, "def inBoundsIdx (i : Int) (n : Nat) : Bool := decide (0 ≤ i) && decide (i < (n : Int))",
         "def inBoundsIdx (i : Int) (n : Nat) : Bool := decide (0 ≤ i) && decide (i ≤ (n : Int))")],
      "the index equal to the length passes the bounds check"),
    M("bounds-negative", "§6.5", "(D-Index-Trap)", "bounds",
      [E(DY, "      if inBoundsIdx i cs.length then", "      if decide (i < (cs.length : Int)) then")],
      "a negative index is not trapped (it reads element 0)"),
    M("bounds-stuck", "§6.5", "(D-Index-Trap)", "bounds",
      [E(DY, "          | .ok ρ => .at ℓ c sub ρ\n          | .bounds => .bounds", "          | .ok ρ => .at ℓ c sub ρ\n          | .bounds => .stuck .typeConfusion")],
      "the bounds trap removed: an out-of-range index is a stuck state"),
    M("repeat-count", "§6.5", "array repeat", "off-by-one",
      [E(DY, "(fun i => .array T i (List.replicate n v))", "(fun i => .array T i (List.replicate (n + 1) v))"),
       E(SP, "(.ret (.array T H.length (List.replicate n v))) tr)", "(.ret (.array T H.length (List.replicate (n + 1) v))) tr)", 2)],
      "eval, Step and step: `[v; n]` builds n + 1 elements"),
    M("overflow-wrap", "§6.4", "(D-Arith-Trap)", "trap",
      [E(DY, "  if InBounds w s n then .val (.int w s n) else .trap .overflow", "  .val (.int w s (wrapInt w s n))")],
      "arithmetic wraps instead of trapping"),
    M("divzero-kind", "§6.4", "(D-Div-Trap)", "trap",
      [E(DY, "  | .div => if n₂ = 0 then .trap .divZero", "  | .div => if n₂ = 0 then .trap .remZero")],
      "division by zero reports the remainder trap"),
    M("rem-min-overflow", "§6.4", "(D-Div-Trap), MIN % -1", "trap",
      [E(DY, "      else if s = .signed ∧ n₁ = intMin w s ∧ n₂ = -1 then .trap .overflow\n", "")],
      "`MIN % -1` yields 0 instead of trapping"),
    M("operand-swap", "§6.4", "(D-Arith)", "operand",
      [E(DY, "if w₁ = w₂ ∧ s₁ = s₂ then binOpInt op w₁ s₁ n₁ n₂ else .confused", "if w₁ = w₂ ∧ s₁ = s₂ then binOpInt op w₁ s₁ n₂ n₁ else .confused")],
      "an integer operator's operands are swapped"),
    M("gt-off-by-one", "§6.4", "(D-Ord)", "off-by-one",
      [E(DY, "  | .gt => .val (.bool (decide (n₂ < n₁)))", "  | .gt => .val (.bool (decide (n₂ ≤ n₁)))")],
      "`>` is `>=`"),
    M("neg-no-overflow", "§6.4", "(D-Neg)", "trap",
      [E(DY, "  | .neg, .int w s n => intResult w s (-n)", "  | .neg, .int w s n => .val (.int w s (-n))")],
      "`-MIN` is not trapped"),
    M("cast-kind", "§6.4", "(D-Int-Cast-Trap)", "trap",
      [E(DY, "else .trap .castOverflow", "else .trap .overflow")],
      "an int cast out of range reports the arithmetic trap"),
    M("float-to-int-saturate", "§6.4", "(D-Float-To-Int)", "trap",
      [E(DY, "      | none => .trap .overflow", "      | none => .val (.int w' s' 0)")],
      "an out-of-range float-to-int yields 0 instead of trapping"),
    M("binop-eval-order", "§6.2", "evaluation order, eval only", "order",
      [E(DY, "      (eval M fuel P H φ e₁).andThen fun H₁ v₁ =>\n        (eval M fuel P H₁ φ e₂).andThen fun H₂ v₂ =>\n          (evalBinOp M op v₁ v₂).toRes H₂",
         "      (eval M fuel P H φ e₂).andThen fun H₁ v₂ =>\n        (eval M fuel P H₁ φ e₁).andThen fun H₂ v₁ =>\n          (evalBinOp M op v₁ v₂).toRes H₂")],
      "eval only: a binary operator's right operand is evaluated first"),
    M("index-write-order", "§6.2", "evaluation order, eval only", "order",
      [E(DY, "      (eval M fuel P H φ e).andThen fun H₁ v =>\n        match evalArgs (fun H' e' => eval M fuel P H' φ e') H₁ idx with\n        | .abort r => r\n        | .ok H₂ vs tr =>\n          EvalRes.withTrace tr <|\n",
         "        match evalArgs (fun H' e' => eval M fuel P H' φ e') H idx with\n        | .abort r => r\n        | .ok H₁ vs tr =>\n          EvalRes.withTrace tr <| (eval M fuel P H₁ φ e).andThen fun H₂ v =>\n")],
      "eval only: `a[i] = e` evaluates the index before the right-hand side"),
    M("dtor-skip", "§6.11", "drop glue: destructor", "drop-skip",
      [E(DY, "              .ok ((if sd.dtor then [Event.dtor s (.struct s i cs)] else []) ++ evs)", "              .ok evs"),
       E(DY, "       | some sd => if sd.dtor then [Event.dtor s (.struct s i cs)] else []\n       | none => []) ++ dropEventsList D cs",
         "       | some _ => []\n       | none => []) ++ dropEventsList D cs")],
      "a struct's destructor never runs (dropContents and dropEvents)"),
    M("dtor-after-fields", "§6.11", "drop glue order (3.9:15)", "drop-order",
      [E(DY, "              .ok ((if sd.dtor then [Event.dtor s (.struct s i cs)] else []) ++ evs)",
         "              .ok (evs ++ (if sd.dtor then [Event.dtor s (.struct s i cs)] else []))"),
       E(DY, "      (match D.structs[s]? with\n       | some sd => if sd.dtor then [Event.dtor s (.struct s i cs)] else []\n       | none => []) ++ dropEventsList D cs",
         "      dropEventsList D cs ++ (match D.structs[s]? with\n       | some sd => if sd.dtor then [Event.dtor s (.struct s i cs)] else []\n       | none => [])")],
      "fields are dropped before the destructor runs"),
    M("fields-reverse", "§6.11", "drop glue order (3.9:15)", "drop-order",
      [E(DY, "          match dropContentsList D cs with\n          | .error w => .error w\n          | .ok evs' => .ok (evs ++ evs')",
         "          match dropContentsList D cs with\n          | .error w => .error w\n          | .ok evs' => .ok (evs' ++ evs)"),
       E(DY, "  | c :: cs => dropEvents D c ++ dropEventsList D cs", "  | c :: cs => dropEventsList D cs ++ dropEvents D c")],
      "fields and elements are dropped last to first"),
    M("scope-fifo", "§6.9", "frame exit drop order", "drop-order",
      [E(DY, "  unwindLocs D H φ.scope.reverse", "  unwindLocs D H φ.scope"),
       E(SP, "plainUnwind P.decls H φ.scope.reverse", "plainUnwind P.decls H φ.scope", 4)],
      "eval, Step and step: a frame's bindings are dropped first-declared first"),
    M("payload-order", "§6.6", "match arm exit order", "drop-order",
      [E(DY, "                 match unwindLocs P.decls H₂ minted.2.reverse with", "                 match unwindLocs P.decls H₂ minted.2 with"),
       E(SP, "plainUnwind P.decls H ℓs.reverse", "plainUnwind P.decls H ℓs", 2)],
      "eval, Step and step: an arm's payload bindings are dropped first to last"),
    M("overwrite-no-drop", "§6.8", "(D-Assign) overwrite drop", "drop-skip",
      [E(DY, "if c'.copyClosed P.decls then .ok (H₁.set ℓ (.full c')) .unit evs", "if c'.copyClosed P.decls then .ok (H₁.set ℓ (.full c')) .unit []"),
       E(SP, "      Step M P (.run H φ (.assign p :: K) (.ret v) tr)\n        (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))",
         "      Step M P (.run H φ (.assign p :: K) (.ret v) tr)\n        (.run (H.set ℓ (.full c')) φ K (.ret .unit) tr)"),
       E(SP, "          | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret .unit) (tr ++ evs))\n  | .ret =>",
         "          | some c' => .next (.run (H.set ℓ (.full c')) φ K (.ret .unit) tr)\n  | .ret =>")],
      "eval, Step and step: an assignment's old value is never dropped"),
    M("break-skip-local", "§6.10", "(D-Break) unwind", "off-by-one",
      [E(DY, "unwindLocs P.decls H₁ (sc.drop φ.scope.length).reverse", "unwindLocs P.decls H₁ (sc.drop (φ.scope.length + 1)).reverse"),
       E(SP, "      Kont.toLoop K = some (φs, K') →\n      plainUnwind P.decls H (φ.scope.drop φs.scope.length).reverse = .ok (H', evs) →",
         "      Kont.toLoop K = some (φs, K') →\n      plainUnwind P.decls H (φ.scope.drop (φs.scope.length + 1)).reverse = .ok (H', evs) →"),
       E(SP, "    | some (φs, K') =>\n      match plainUnwind P.decls H (φ.scope.drop φs.scope.length).reverse with",
         "    | some (φs, K') =>\n      match plainUnwind P.decls H (φ.scope.drop (φs.scope.length + 1)).reverse with")],
      "eval, Step and step: `break` skips the outermost loop-local's drop"),
    M("seq-affine-as-linear", "§6.7", "(D-Seq) affine discard", "affine-linear",
      [E(DY, "        | .affine =>\n            (match dropContents P.decls (Contents.ofVal v₁) with",
         "        | .affine =>\n            (match (Except.error .linearDiscard : Except Violation (List Event)) with")],
      "eval only: discarding an affine temporary is refused like a linear one"),
    M("seq-droptemp-skip", "§6.7", "(D-Seq) temporary drop mark", "drop-skip",
      [E(DY, "(eval M fuel P H₁ φ e₂).withTrace (.dropTemp v₁ :: evs))", "(eval M fuel P H₁ φ e₂).withTrace evs)"),
       E(SP, "(.eval e₂) (tr ++ (.dropTemp v :: evs)))", "(.eval e₂) (tr ++ evs))", 2)],
      "eval, Step and step: a discarded temporary's drop is not marked"),
    M("residue-mark-skip", "§6.3", "destructure residue drop mark", "drop-skip",
      [E(DY, "  if r.mult D = .copy then [] else [.drop ℓ r]", "  []")],
      "a destructure's residue drops are not marked"),
    M("match-consume-skip", "§6.6", "(D-Match) consume", "drop-skip",
      [E(DY, "  if D.enumClassOf e = .copy then [] else [.consume (.enum e k i (vs.map fun _ => .hole))]", "  []")],
      "a matched non-Copy enum's shell is not consumed"),
    M("leak-monitor-off", "§6.11", "linearLeak monitor", "monitor",
      [E(DY, "      if c.residualLinear D then .error .linearLeak", "      if false then .error .linearLeak"),
       E(DY, "      if r.residualLinear D then .error .linearLeak", "      if false then .error .linearLeak")],
      "the machine no longer refuses to drop a live linear value"),
    M("overwrite-monitor-off", "§6.8", "linearOverwrite monitor", "monitor",
      [E(DY, "                if old.residualLinear P.decls then .stuck .linearOverwrite   -- 3.8:77",
         "                if false then .stuck .linearOverwrite   -- 3.8:77")],
      "the machine no longer refuses to overwrite a live linear value"),
    M("discard-monitor-off", "§6.7", "linearDiscard monitor", "monitor",
      [E(DY, "        | .linear => .stuck .linearDiscard                         -- 3.8:64",
         "        | .linear => eval M fuel P H₁ φ e₂")],
      "the machine no longer refuses to discard a linear temporary"),
    M("copy-monitor-off", "§6.5", "ownedUnderCopy monitor", "monitor",
      [E(DY, "  if (Contents.ofVal (mk H.length)).copyClosed D then", "  if true then")],
      "the machine no longer refuses an owned value under a Copy aggregate"),
    M("dyn-residual-declared", "§6.11", "Contents.residualLinear (3.8:74)", "affine-linear",
      [E(DY, "       | some sd => sd.attr = .linear || Contents.residualLinearList D cs",
         "       | some sd => Contents.residualLinearList D cs")],
      "the machine treats a declared-linear struct's obligation as its fields'"),
]




# ---------------------------------------------------------------------------------------
# The proofs-off copy: every theorem of layers 0-3 (`PROOF_FILES`) with its proof replaced
# by `sorry`, and the witnesses-off copy: every theorem, example and `#guard` of layer 4 too.
# ---------------------------------------------------------------------------------------

THEOREM = re.compile(r"(?m)^(?:private |protected )?theorem ([^\s:({]+)")
EXAMPLE = re.compile(r"(?m)^(?:private |protected )?(?:theorem ([^\s:({]+)|example\b)")


def comment_mask(t):
    """1 at every character inside a comment or a string literal."""
    m = bytearray(len(t))
    i, n, depth = 0, len(t), 0
    while i < n:
        if depth == 0 and t.startswith("--", i):
            j = t.find("\n", i)
            j = n if j < 0 else j
            m[i:j] = b"\1" * (j - i)
            i = j
        elif t.startswith("/-", i):
            depth += 1
            m[i:i + 2] = b"\1\1"
            i += 2
        elif depth and t.startswith("-/", i):
            depth -= 1
            m[i:i + 2] = b"\1\1"
            i += 2
        elif depth:
            m[i] = 1
            i += 1
        elif t[i] == '"':
            j = i + 1
            while j < n and t[j] != '"':
                j += 2 if t[j] == "\\" else 1
            m[i:j + 1] = b"\1" * (j + 1 - i)
            i = j + 1
        else:
            i += 1
    return m


def decl_count(t, witnesses=False):
    """The top-level declarations `sorry_proofs` must turn off: every `theorem`, and with
    `witnesses` every `example` and `#guard` as well (outside comments and strings)."""
    mask = comment_mask(t)
    n = sum(1 for mm in (EXAMPLE if witnesses else THEOREM).finditer(t) if not mask[mm.start()])
    return n + (len(re.findall(r"(?m)^#guard ", t)) if witnesses else 0)


def sorry_proofs(t, witnesses=False, stats=None):
    """Replace the proof of every top-level `theorem` with `by sorry`, keeping its statement;
    with `witnesses`, every `example` too, and comment out every `#guard`. `stats`, when a
    list, receives the number of declarations turned off."""
    mask = comment_mask(t)
    out, pos, done = [], 0, 0
    if witnesses:
        t, done = re.subn(r"(?m)^#guard .*$", lambda g: "-- " + g.group(0), t)
        mask = comment_mask(t)
    for mm in (EXAMPLE if witnesses else THEOREM).finditer(t):
        s = mm.start()
        if s < pos or mask[s]:
            continue
        # The declaration ends at the next column-0 line outside a comment (or a docstring).
        e, k = len(t), t.find("\n", s)
        while k != -1 and k + 1 < len(t):
            c = t[k + 1]
            if (not mask[k + 1] and c not in " \t\n|)") or t.startswith("/-", k + 1):
                e = k + 1
                break
            k = t.find("\n", k + 1)
        # The proof starts at the first `:=`, `where` or equation `|` outside brackets.
        d, i, b = 0, mm.end(), None
        while i < e:
            if mask[i]:
                i += 1
                continue
            ch = t[i]
            if ch in "([{⟨":
                d += 1
            elif ch in ")]}⟩":
                d -= 1
            elif d == 0 and t.startswith(":=", i):
                b = i
            elif d == 0 and t.startswith(" where", i) and not (t[i + 6:i + 7].isalnum() or t[i + 6:i + 7] == "_"):
                b = i
            elif d == 0 and ch == "\n" and re.match(r"\n[ \t]+\|", t[i:i + 40]):
                b = i
            if b is not None:
                break
            i += 1
        if b is None:
            raise ValueError("no proof found for " + mm.group(0))
        out += [t[pos:b], " := by sorry\n\n"]
        pos = e
        done += 1
    out.append(t[pos:])
    if stats is not None:
        stats.append(done)
    return "".join(out)


# ---------------------------------------------------------------------------------------

def run(cmd, cwd, timeout=None):
    t = time.time()
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr, time.time() - t
    except subprocess.TimeoutExpired:
        return 124, "", "timeout", time.time() - t


def mutated_texts(src_dir, m):
    """The mutant's edited files, as {path: text}; each edit must match exactly `count` times."""
    texts = {}
    for e in m["edits"]:
        f = e["file"]
        if f not in texts:
            texts[f] = open(os.path.join(src_dir, f), encoding="utf-8").read()
        n = texts[f].count(e["old"])
        if n != e["count"]:
            raise ValueError(f"{m['id']}: {f}: expected {e['count']} occurrence(s), found {n}: {e['old'][:70]!r}")
        texts[f] = texts[f].replace(e["old"], e["new"])
    return texts


def write_texts(pkg, texts, level):
    """Write sources at a level: 0 as they are, 1 with the proofs of layers 0-3 off, 2 with
    the witnesses (layer 4's theorems, examples and `#guard`s, and layers 0-3's examples) off
    as well."""
    for f, t in texts.items():
        if level >= 1 and f in PROOF_FILES:
            t = sorry_proofs(t, witnesses=level >= 2)
        elif level >= 2 and f in WITNESS_FILES:
            t = sorry_proofs(t, witnesses=True)
        open(os.path.join(pkg, f), "w", encoding="utf-8").write(t)


def copy_pristine(pristine, pkg, files, level):
    for f in files:
        t = open(os.path.join(pristine, f), encoding="utf-8").read()
        write_texts(pkg, {f: t}, level)


def enclosing_decl(path, line):
    try:
        lines = open(path, encoding="utf-8").read().splitlines()
    except OSError:
        return "?"
    pat = re.compile(r"^\s*(?:@\[[^\]]*\]\s*)?(?:private |protected |noncomputable )*"
                     r"(theorem|def|example|instance|abbrev|#guard|structure|inductive)\b\s*([^\s:({]*)")
    for i in range(min(line, len(lines)) - 1, -1, -1):
        mm = pat.match(lines[i])
        if mm:
            return (mm.group(1) + " " + mm.group(2)).strip()
    return "?"


def classify_build(pkg, log, level):
    errs = re.findall(r"^error: (RueCore/[^:]+\.lean):(\d+):\d+: (.*)$", log, re.M)
    if not errs:
        return "build-error", log[-300:], []
    sites = [{"file": f, "line": int(ln), "decl": enclosing_decl(os.path.join(pkg, f), int(ln)),
              "msg": msg[:120]} for f, ln, msg in errs]

    def witness(s):
        return LAYER_OF.get(s["file"]) in WITNESS_LAYERS or s["decl"].startswith(("example", "#guard"))
    if all(witness(s) for s in sites):
        kind = "witness"
    elif not level:
        kind = "proof"
    elif all(witness(s) or LAYER_OF.get(s["file"]) in DEFINITION_LAYERS for s in sites):
        # With the proofs off, a failure in layers 0-1 is a definition that no longer
        # elaborates (a termination proof, say).
        kind = "definition"
    else:
        # A proof in layers 2-3 failed although every proof there is off: the proofs-off copy
        # is wrong (a module missing from `PROOF_FILES`), not the mutant.
        raise ScriptError(f"proofs-off build failed in a proof module: {sites[0]}")
    first = next(s for s in sites if kind == "witness" or not witness(s))
    where = f"{first['file'].replace('RueCore/', '')}:{first['line']} {first['decl']}"
    if kind == "witness" and all(s["file"] in MIRROR_FILES for s in sites):
        kind = "mirror"
    return kind, where, sites


class ScriptError(Exception):
    pass


def corpus(pkg, args):
    rc, out, err, _ = run(["lake", "exe", "ruecore-corpus"] + args, pkg, timeout=1800)
    if rc != 0:
        return None, (err.strip().splitlines() or [f"exit {rc}"])[-1:]
    return {c["name"]: c for c in json.loads(out)}, None


def case_key(c):
    return json.dumps({"verdict": c["verdict"], "expected": c["expected"]}, sort_keys=True)


def diff_cases(base, mut):
    names = sorted(set(base) | set(mut))
    return [n for n in names if n not in base or n not in mut or case_key(base[n]) != case_key(mut[n])]


def unsound(cases, names):
    """Changed cases the mutant's checker accepts and the machine refuses: a counterexample to
    `check_sound` + soundness for the mutated definitions, whatever the proofs say."""
    return [n for n in names if n in cases and "accept" in cases[n]["verdict"]
            and cases[n]["expected"].get("kind") == "stuck"]


def compiler_disagrees(root, case, outdir):
    """The state directory's bin/verify.py comparison, for one case."""
    path = os.path.join(outdir, case["name"] + ".rue")
    open(path, "w").write(case["source"])
    p = subprocess.run([os.path.join(root, "scripts/rue"), "exec", path], capture_output=True, text=True, cwd=root)
    stdout = [l for l in p.stdout.splitlines() if not l.startswith("Compiled ")]
    exp = case["expected"]
    if "accept" not in case["verdict"]:
        return p.returncode != 1
    if exp["kind"] == "ok":
        return not (p.returncode == exp["exit"] and stdout == exp["stdout"])
    if exp["kind"] == "panic":
        return not (p.returncode == 101 and stdout == exp.get("stdout", []))
    return True


def test(pkg, base, a, outdir, level):
    """Build the mutated package and run the suite on it: (result dict)."""
    r = {}
    rc, out, err, bsecs = run(["lake", "build", "RueCore", "ruecore-corpus"], pkg, timeout=3600)
    r["build_s"] = round(bsecs)
    if rc != 0:
        kind, where, sites = classify_build(pkg, out + err, level)
        r.update(killed=kind, detail=where, sites=sites[:8])
        return r
    seeds, e = corpus(pkg, [])
    if e:
        r.update(killed="corpus", detail="ruecore-corpus failed: " + " ".join(e))
        return r
    d = diff_cases(base["seeds"], seeds)
    if d:
        r.update(killed="corpus", detail=f"{len(d)} seed(s): " + ", ".join(d[:6]) + (" …" if len(d) > 6 else ""),
                 cases=d, unsound=unsound(seeds, d))
        return r
    gen, e = corpus(pkg, ["--gen", str(a.gen), "--seed", str(a.gen_seed)])
    if e:
        r.update(killed="bridge", detail="generator failed: " + " ".join(e))
        return r
    gd = [n for n in diff_cases(base["gen"], gen) if n not in base["seeds"]]
    r.update(gen_changed=gd, unsound=unsound(gen, gd))
    if not gd:
        r.update(killed="survived", detail=f"seeds and {a.gen} generated (seed {a.gen_seed}) unchanged")
    elif not a.compiler_root:
        r.update(killed="gen-changed", detail=f"{len(gd)} generated case(s) changed; no compiler run: " + ", ".join(gd[:6]))
    else:
        os.makedirs(outdir, exist_ok=True)
        bad = [n for n in gd if n in gen and compiler_disagrees(a.compiler_root, gen[n], outdir)]
        if bad:
            r.update(killed="bridge", detail=f"compiler disagrees on {len(bad)} of {len(gd)} changed generated case(s): " + ", ".join(bad[:6]))
        else:
            r.update(killed="survived", detail=f"{len(gd)} generated case(s) changed, the compiler agrees with the mutant on each")
    return r


def baseline(pkg, a):
    rc, out, err, secs = run(["lake", "build", "RueCore", "ruecore-corpus"], pkg)
    if rc != 0:
        sys.exit(f"baseline build failed in {pkg}:\n" + (out + err)[-2000:])
    seeds, e1 = corpus(pkg, [])
    gen, e2 = corpus(pkg, ["--gen", str(a.gen), "--seed", str(a.gen_seed)])
    if e1 or e2:
        sys.exit(f"baseline corpus failed in {pkg}: {e1} {e2}")
    return {"seeds": seeds, "gen": gen}, secs


def check(src):
    """What `--check` verifies, as a list of one-line errors: every package module is in
    Layers.lean's table and every table entry is a module; every mutant's edits apply exactly
    and touch only layers 0-1; and in every module the proofs-off copy turns off every theorem
    (and the witnesses-off copy every example and `#guard`), and doing it twice changes nothing but whitespace."""
    errs = []
    files = {os.path.relpath(os.path.join(r, f), src) for r, _, fs in os.walk(os.path.join(src, "RueCore"))
             for f in fs if f.endswith(".lean")} | {"RueCore.lean"}
    for f in sorted(files - set(LAYER_OF)):
        errs.append(f"{f} is not in RueCore/Layers.lean's table, so no pass knows what it is")
    for f in sorted(set(LAYER_OF) - files):
        errs.append(f"{f} is in RueCore/Layers.lean's table but not in the package")
    if errs:
        return errs
    for m in MUTANTS:
        try:
            texts = mutated_texts(src, m)
        except ValueError as e:
            errs.append(str(e))
            continue
        for f in texts:
            if f not in DEFINITION_FILES:
                errs.append(f"{m['id']} edits {f}, which is not in layers {DEFINITION_LAYERS}")
    for f in PROOF_FILES + WITNESS_FILES:
        t = open(os.path.join(src, f), encoding="utf-8").read()
        for w in ((False, True) if f in PROOF_FILES else (True,)):
            st = []
            try:
                once = sorry_proofs(t, witnesses=w, stats=st)
            except ValueError as e:
                errs.append(f"{f}: {e}")
                continue
            want = decl_count(t, witnesses=w)
            if st[0] != want:
                errs.append(f"{f}: turned off {st[0]} of {want} declarations ({'witnesses' if w else 'proofs'} off)")
            ws = lambda x: re.sub(r"\s+", " ", x)
            if ws(sorry_proofs(once, witnesses=w)) != ws(once):
                errs.append(f"{f}: turning the {'witnesses' if w else 'proofs'} off twice changes the file")
    return errs


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--src", default=DEFAULT_SRC)
    ap.add_argument("--work")
    ap.add_argument("--only")
    ap.add_argument("--compiler-root")
    ap.add_argument("--gen", type=int, default=200)
    ap.add_argument("--gen-seed", type=int, default=7)
    ap.add_argument("--lake-cache")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--check", action="store_true", help="only check that every edit applies")
    ap.add_argument("--table", action="store_true", help="print the results as Markdown")
    ap.add_argument("--redo", action="store_true", help="rerun mutants that already have a result")
    a = ap.parse_args()
    src = os.path.abspath(a.src)
    configure(src)
    ids = [m["id"] for m in MUTANTS]
    assert len(ids) == len(set(ids)), "duplicate mutant id"
    if a.list:
        for m in MUTANTS:
            print(f"{m['id']:26} {m['sect']:10} {m['op']:20} {m['rule']}")
        print(f"{len(MUTANTS)} mutants")
        return 0
    if a.check:
        errs = check(src)
        for e in errs:
            print("mutate.py --check: " + e)
        if errs:
            return 1
        print(f"{len(MUTANTS)} mutants apply cleanly to {src}; {len(LAYER_OF)} modules classified "
              f"({len(DEFINITION_FILES)} definition, {len(PROOF_FILES)} proof-bearing, {len(WITNESS_FILES)} tooling), "
              "every theorem, example and #guard turned off by the proofs-off and witnesses-off copies")
        return 0
    if not a.work:
        ap.error("--work is required")
    work = os.path.abspath(a.work)
    if work == src or work.startswith(src + os.sep) or src.startswith(work + os.sep):
        ap.error("--work must lie outside the package")
    resf = os.path.join(work, "results.json")
    results = json.load(open(resf)) if os.path.exists(resf) else {}
    if a.table:
        print_table(results)
        return 0
    os.makedirs(work, exist_ok=True)
    # A pristine copy of the sources (the package itself is never written), then one scratch
    # package per level: 0 as it is, 1 with the L0-L2 proofs off, 2 with the witnesses off too.
    pristine = os.path.join(work, "pristine")
    if os.path.exists(pristine):
        shutil.rmtree(pristine)
    shutil.copytree(src, pristine, ignore=shutil.ignore_patterns(".lake", "bin", "__pycache__"))
    allfiles = [os.path.relpath(os.path.join(r, f), pristine) for r, _, fs in os.walk(pristine) for f in fs]
    pkgs = {0: os.path.join(work, "pkg"), 1: os.path.join(work, "pkg-noproofs"), 2: os.path.join(work, "pkg-corpusonly")}
    todo = [m for m in MUTANTS if (not a.only or m["id"] in a.only.split(","))
            and (a.redo or m["id"] not in results or pending(results[m["id"]]))]
    bases = {}
    for lv, pkg in pkgs.items():
        if not os.path.exists(pkg):
            shutil.copytree(pristine, pkg)
            if a.lake_cache:
                shutil.copytree(os.path.join(a.lake_cache, ".lake"), os.path.join(pkg, ".lake"))
        # Sources afresh: a reused package may hold modules the current sources no longer have.
        for d in os.listdir(pkg):
            if d != ".lake":
                p = os.path.join(pkg, d)
                shutil.rmtree(p) if os.path.isdir(p) else os.remove(p)
        shutil.copytree(pristine, pkg, dirs_exist_ok=True)
        copy_pristine(pristine, pkg, [f for f in allfiles if f in LAYER_OF], lv)
        bases[lv], secs = baseline(pkg, a)
        print(f"baseline, {LEVELS[lv]}: {len(bases[lv]['seeds'])} seeds, "
              f"{len(bases[lv]['gen']) - len(bases[lv]['seeds'])} generated at seed {a.gen_seed}, build {secs:.0f}s", flush=True)
        if diff_cases(bases[0]["gen"], bases[lv]["gen"]):
            sys.exit(f"the {LEVELS[lv]} baseline's corpus differs from the baseline's")
    try:
        run_mutants(a, todo, results, resf, pristine, pkgs, bases, work)
    except ScriptError as e:
        print(f"mutate.py: script error, stopping: {e}", flush=True)
        return 1
    return 0


def run_mutants(a, todo, results, resf, pristine, pkgs, bases, work):
    for m in todo:
        t0 = time.time()
        texts = mutated_texts(pristine, m)
        r = {} if a.redo else results.get(m["id"], {})
        r.update(id=m["id"], sect=m["sect"], rule=m["rule"], op=m["op"], note=m["note"])
        lv = 0
        while lv is not None:
            key = PASS_KEYS[lv]
            if a.redo or not (r if lv == 0 else r.get(key, {})).get("killed"):
                write_texts(pkgs[lv], texts, lv)
                try:
                    res = test(pkgs[lv], bases[lv], a, os.path.join(work, "cases", m["id"]), lv)
                finally:
                    copy_pristine(pristine, pkgs[lv], list(texts), lv)
                if lv == 0:
                    r.update(res)
                else:
                    r[key] = res
            lv = next_level(r, lv)
        r["total_s"] = r.get("total_s", 0) + round(time.time() - t0) if not a.redo else round(time.time() - t0)
        results[m["id"]] = r
        json.dump(results, open(resf, "w"), indent=1, ensure_ascii=False)
        line = f"{m['id']:26} {r['killed']:9} {r.get('detail', '')[:100]}"
        for k in (PASS_KEYS[1], PASS_KEYS[2]):
            if k in r:
                line += f"\n{'':26} {k}: {r[k]['killed']}  {r[k].get('detail', '')[:100]}"
        print(line, flush=True)
    return 0


LEVELS = {0: "full", 1: "proofs off", 2: "proofs and witnesses off"}
PASS_KEYS = {0: "", 1: "without_proofs", 2: "corpus_only"}


def next_level(r, lv):
    """The next pass a mutant needs: after a proof kill, the proofs-off pass; after a witness
    kill (at either level), the pass with the witnesses off too."""
    k = r["killed"] if lv == 0 else r[PASS_KEYS[lv]]["killed"]
    if lv == 0 and k == "proof":
        return 1
    if lv < 2 and k in ("witness", "mirror"):
        return 2
    return None


def pending(r):
    lv = 0
    if not r.get("killed"):
        return True
    while True:
        lv = next_level(r, lv)
        if lv is None:
            return False
        if PASS_KEYS[lv] not in r:
            return True


def print_table(results):
    order = {m["id"]: i for i, m in enumerate(MUTANTS)}
    print("| # | Mutant | § | Rule | Operator | Killed by | Without the proofs | Corpus and bridge alone | Time (s) |")
    print("|---|---|---|---|---|---|---|---|---|")
    for i, (k, r) in enumerate(sorted(results.items(), key=lambda kv: order.get(kv[0], 999)), 1):
        np = r.get("without_proofs", {}).get("killed", "")
        co = r.get("corpus_only", {}).get("killed", "")
        print(f"| {i} | `{k}` | {r['sect']} | {r['rule']} | {r['op']} | {r['killed']} | {np} | {co} | {r.get('total_s', '')} |")


if __name__ == "__main__":
    sys.exit(main())
