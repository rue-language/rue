import RueCore.Explain

/-!
# RueCore.Explain.Text — the terminal rendering (RUE-2246)

One plain-text page per program: the program in Rue surface syntax, the
checker's verdict with the failing premise named when it rejects, the §5
derivation of every function body as an indented tree carrying the fused
`Γ;Σ` at every node, and the §6 run as one step table in execution order,
spanning frames, with the store before and after each node and its drop
events marked.

In an editor, `#eval IO.println (Text.render "demo" "" [] P)` prints the page
for any fragment program `P`, the way `Examples.lean`'s `#eval` lines run the
interpreter.

Nothing here is trusted: a function's verdict is `Deriv.result`, which
`explain_result` proves equal to `check`, and the outcome is `Trace.res`,
which `traceEval_res` proves equal to `eval`.
-/

namespace RueCore
namespace Explain
namespace Text

/-- (helper) A run of one character, for the section rules. -/
def bar (c : Char) (n : Nat) : String := "".pushn c n

/-- (helper) `n` spaces. -/
def sp (n : Nat) : String := "".pushn ' ' n

/-- (helper) Pad a label out to `n` columns so the two-column blocks line
up. -/
def pad (n : Nat) (s : String) : String :=
  if s.length ≥ n then s else s ++ sp (n - s.length)

/-- (helper) Greedy word wrap, so a description or a premise reads as a
paragraph rather than one very long line. -/
def wrap (width : Nat) (s : String) : List String :=
  let step := fun (acc : List String × String) (w : String) =>
    let (lines, cur) := acc
    if cur.isEmpty then (lines, w)
    else if cur.length + 1 + w.length ≤ width then (lines, cur ++ " " ++ w)
    else (lines ++ [cur], w)
  let (lines, cur) := (s.splitOn " ").foldl step ([], "")
  if cur.isEmpty then lines else lines ++ [cur]

/-- (helper) A wrapped paragraph, every line given the same indent. -/
def para (indent width : Nat) (s : String) : List String :=
  (wrap width s).map (fun l => sp indent ++ l)

/-- (helper) A section heading. -/
def section' (title : String) : List String :=
  ["", "── " ++ title ++ " " ++ bar '─' (76 - title.length), ""]

/-! ## The derivation -/

/-- The §5 judgment's conclusion at one node, as the calculus writes its
right-hand side: `⇒ T ⊣ Σ'`, or the premise that failed. -/
def verdictLines (indent : Nat) : Verdict → List String
  | .accept T Γ' =>
      [sp indent ++ pad 8 "⇒" ++ Print.tyName T ++ "  ⊣  " ++ clip 80 (ctxLine Γ')]
  | .reject why =>
      (sp indent ++ pad 8 "✗" ++ "this premise fails:") ::
        para (indent + 8) 68 why

/-- One node of the derivation tree, indented by its depth: the §5 rule the
node applies, the expression it concludes about, the incoming fused `Γ;Σ`,
and its conclusion. Children (the rule's premises, in premise order) follow,
indented one level further. -/
partial def derivLines (P : Program) (R : Ty) (indent : Nat) : Deriv → List String
  | .node r Γ e v kids =>
      let binders := binderTys Γ
      (sp indent ++ r) ::
      (sp (indent + 2) ++ pad 8 "e" ++ clip 80 (exprLine P R binders e)) ::
      (sp (indent + 2) ++ pad 8 "Γ;Σ" ++ clip 80 (ctxLine Γ)) ::
      verdictLines (indent + 2) v ++
      (kids.map (derivLines P R (indent + 4))).flatten

/-! ## The run -/

/-- What one step produced, for the step table's last line: a value, a value
an unwinding `return` handed past the node (§6.9), a defined trap (§6.12),
one of §6's named refusals, or the interpreter's admission that it ran out
of fuel. -/
def stepResLines (indent : Nat) : StepRes → List String
  | .value v => [sp indent ++ pad 8 "result" ++ "value " ++ valLine v]
  | .unwound v =>
      [sp indent ++ pad 8 "result" ++ "value " ++ valLine v ++
        " — handed past this node by a `return` (§6.9)"]
  | .panicked k =>
      [sp indent ++ pad 8 "result" ++ "PANIC (" ++ Corpus.panicName k ++ ") — a defined trap, §6.12"]
  | .refuse w why =>
      (sp indent ++ pad 8 "result" ++ "REFUSED (" ++ Corpus.violationName w ++ ")") ::
        para (indent + 8) 68 why
  | .exhausted =>
      [sp indent ++ pad 8 "result" ++ "OUT OF FUEL — the interpreter stopped early"]

/-- One row of the step table: its number, the node's depth, the §6 rule it
took, the expression, the store before and after, the drop events the node
emitted (marked `>>`), and what it produced. -/
def stepLines (P : Program) (n : Nat) (s : Step) : List String :=
  let head := "  [" ++ pad 4 (toString n ++ "]") ++ " d" ++ toString s.depth ++ "  " ++
    sp (2 * min s.depth 8) ++ s.rule
  let ind := 9
  head ::
  (sp ind ++ pad 8 "expr" ++ clip 80 (s.text P)) ::
  (sp ind ++ pad 8 "store" ++ clip 72 (storeLine s.storeBefore)) ::
  (sp ind ++ pad 8 "  →" ++ clip 72 (storeLine s.storeAfter)) ::
  (if s.events.isEmpty then []
   else [sp ind ++ pad 8 "events" ++ ">> " ++ eventsLine s.events]) ++
  stepResLines ind s.res

/-- The machine's outcome in one line: a value with its drop trace, a
defined trap (§6.12), the refusal the machine reached (§6's stuck states,
which `soundness` proves unreachable for a well-formed program), or
exhausted fuel. -/
def outcomeLines : EvalRes → List String
  | .ok _ v tr =>
      ["Outcome: ok — value " ++ valLine v,
       "         drop trace: " ++ (if tr.isEmpty then "(no drops)" else eventsLine tr)]
  | .returned _ v tr =>
      ["Outcome: ok — value " ++ valLine v ++ " (handed back by a `return`, §6.9)",
       "         drop trace: " ++ (if tr.isEmpty then "(no drops)" else eventsLine tr)]
  | .panic k => ["Outcome: PANIC (" ++ Corpus.panicName k ++ ") — a defined trap, §6.12"]
  | .stuck w =>
      ("Outcome: REFUSED (" ++ Corpus.violationName w ++ ")") :: para 9 68 (violationPremise w)
  | .outOfFuel =>
      ["Outcome: OUT OF FUEL — the interpreter stopped before the program did.",
       "         `fuel_mono` says a larger bound never changes an answer, so this",
       "         is a bound too small, not a claim about the program."]

/-! ## The page -/

/-- (helper) One line of the verdict block per function: its signature and
what the checker concluded about its body. -/
def fnVerdictLines (P : Program) : List (Nat × FnDef × Deriv) → List String
  | [] => []
  | (i, fd, d) :: rest =>
      let head := sp 9 ++ fnHeader i fd
      let tail := match d.result with
        | some (T, Γf) =>
            if T = fd.ret ∧ NoOwnedLinear P.structs Γf then
              [head ++ "  — body ⇒ " ++ Print.tyName T ++ ", exit Σ " ++ clip 40 (ctxLine Γf)]
            else if T = fd.ret then
              [head ++ "  — REJECTED: a by-value parameter or a still-open binding is",
               sp 11 ++ "still Owned at a linear type where the body ends ((Fn) §5.8's",
               sp 11 ++ "second clause; 3.8:62; the compiler reports E0406)"]
            else
              [head ++ "  — REJECTED: the body has type " ++ Print.tyName T ++
                 ", not the declared return type"]
        | none => [head ++ "  — REJECTED: see the derivation below"]
      tail ++ fnVerdictLines P rest

/-- The checker's verdict (§5) for the whole program, with the failing
premise stated prominently when it rejects: the rule it belongs to, the
subexpression it was checked on, and the premise in the calculus's own words
with its citation. -/
def verdictSection (P : Program) (ds : List (Nat × FnDef × Deriv)) : List String :=
  let failing := (ds.map (fun t => (deepestFailure t.2.2).map (fun f => (t.1, t.2.1, f)))).reduceOption
  if checkProgram P then
    ["Verdict: ACCEPTED by the checker (§5).",
     "         Every function is well-formed by (Fn) §5.8 and the entry point takes",
     "         no parameters, so `checkProgram_sound` ties the whole program to a",
     "         real derivation and the §7 safety theorems apply to it.", ""] ++
    fnVerdictLines P ds
  else
    ["Verdict: REJECTED by the checker (§5).", ""] ++
    (match failing.head? with
     | some (i, fd, (r, e, binders, why)) =>
         ["The premise that fails, in " ++ fnHeader i fd ++ ", at " ++ r ++ ":", "",
          "    on  " ++ clip 74 (exprLine P fd.ret binders e), ""] ++ para 4 74 why
     | none =>
         ["No function body's derivation failed, so the rejection is the whole-program",
          "premise: either a by-value parameter or a body-local binding is still Owned",
          "at a linear type where its function's body ends ((Fn) §5.8's second clause,",
          "3.8:62), or the entry point does not take an empty parameter list.", ""] ++
         fnVerdictLines P ds)

/-- A complete plain-text explanation of one fragment program: its source,
the checker's verdict and per-function derivations (§5), and the machine's
run (§6). -/
def render (name description : String) (rules : List String) (P : Program) : String :=
  let ds := programDerivs P 0 P.fns
  let t := runTrace P Corpus.exportFuel
  let lines :=
    [bar '═' 80, " " ++ name, bar '═' 80] ++
    para 0 78 description ++
    ["", "Rules exercised: " ++ String.intercalate " · " rules] ++
    section' "The program" ++
    (Print.moduleItems P ++ Print.fnItems P 0 P.fns).splitOn "\n" ++
    section' "What the checker says (§5)" ++
    verdictSection P ds ++
    section' "The derivations (§5)" ++
    ["Each node is one rule; `e` is the expression it concludes about, `Γ;Σ` the",
     "fused context flowing into it, and `⇒ T ⊣ Σ'` its conclusion. A rule's",
     "premises are the nodes indented under it, in premise order. One tree per",
     "function body, checked from (Fn) §5.8's entry context."] ++
    (ds.map (fun p => ["", "  " ++ fnHeader p.1 p.2.1, ""] ++
        derivLines P p.2.1.ret 2 p.2.2)).flatten ++
    section' "The run (§6)" ++
    ["One row per evaluated node, in execution order: a node's premises run before",
     "the node itself, so the table reads top to bottom as the machine ran. `d<n>`",
     "is the nesting depth — a callee's rows are deeper than its call's — and `>>`",
     "marks a drop event. The run enters at f0(), as `Dynamics.run` does.", ""] ++
    ((numbered 1 t.steps).map (fun p => stepLines P p.1 p.2)).flatten ++
    ["", bar '─' 80] ++
    outcomeLines t.res ++
    [""]
  String.intercalate "\n" lines ++ "\n"

/-- (helper) The rendering of one bridge corpus case (`Corpus.lean`). -/
def renderCase (c : Corpus.Case) : String :=
  render c.name c.description c.rules c.prog

end Text
end Explain
end RueCore
