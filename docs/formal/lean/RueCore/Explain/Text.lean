import RueCore.Explain

/-!
# RueCore.Explain.Text — the terminal rendering (RUE-2246)

One plain-text page per program: the program in Rue surface syntax, the
checker's verdict with the failing premise named when it rejects, the §5
derivation as an indented tree carrying the fused `Γ;Σ` at every node, and
the §6 run as a step table in execution order with the store before and
after each node and its drop events marked.

In an editor, `#eval IO.println (Text.render "demo" "" [] e)` prints the
page for any fragment expression `e`, the way `Examples.lean`'s `#eval`
lines run the interpreter.

Nothing here is trusted: the verdict is `Deriv.result`, which
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
partial def derivLines (indent : Nat) : Deriv → List String
  | .node r Γ e v kids =>
      let binders := binderTys Γ
      (sp indent ++ r) ::
      (sp (indent + 2) ++ pad 8 "e" ++ clip 80 (exprLine binders e)) ::
      (sp (indent + 2) ++ pad 8 "Γ;Σ" ++ clip 80 (ctxLine Γ)) ::
      verdictLines (indent + 2) v ++
      (kids.map (derivLines (indent + 4))).flatten

/-! ## The run -/

/-- What one step produced, for the step table's last line: a value, a
defined trap (§6.12), or one of §6's named refusals. -/
def stepResLines (indent : Nat) : StepRes → List String
  | .value v => [sp indent ++ pad 8 "result" ++ "value " ++ valLine v]
  | .panicked k =>
      [sp indent ++ pad 8 "result" ++ "PANIC (" ++ Corpus.panicName k ++ ") — a defined trap, §6.12"]
  | .refuse w why =>
      (sp indent ++ pad 8 "result" ++ "REFUSED (" ++ Corpus.violationName w ++ ")") ::
        para (indent + 8) 68 why

/-- One row of the step table: its number, the node's depth, the §6 rule it
took, the expression, the store before and after, the drop events the node
emitted (marked `>>`), and what it produced. -/
def stepLines (n : Nat) (s : Step) : List String :=
  let head := "  [" ++ pad 4 (toString n ++ "]") ++ " d" ++ toString s.depth ++ "  " ++
    sp (2 * min s.depth 8) ++ s.rule
  let ind := 9
  head ::
  (sp ind ++ pad 8 "expr" ++ clip 80 (exprLine s.binders s.expr)) ::
  (sp ind ++ pad 8 "store" ++ clip 72 (storeLine s.storeBefore)) ::
  (sp ind ++ pad 8 "  →" ++ clip 72 (storeLine s.storeAfter)) ::
  (if s.events.isEmpty then []
   else [sp ind ++ pad 8 "events" ++ ">> " ++ eventsLine s.events]) ++
  stepResLines ind s.res

/-- The machine's outcome in one line: a value with its drop trace, a
defined trap (§6.12), or the refusal the machine reached (§6's stuck
states, which `soundness` proves unreachable for a well-typed program). -/
def outcomeLines : EvalRes → List String
  | .ok _ v tr =>
      ["Outcome: ok — value " ++ valLine v,
       "         drop trace: " ++ (if tr.isEmpty then "(no drops)" else eventsLine tr)]
  | .panic k => ["Outcome: PANIC (" ++ Corpus.panicName k ++ ") — a defined trap, §6.12"]
  | .stuck w =>
      ("Outcome: REFUSED (" ++ Corpus.violationName w ++ ")") :: para 9 68 (violationPremise w)

/-! ## The page -/

/-- The checker's verdict (§5), with the failing premise stated prominently
when it rejects: the rule it belongs to, the subexpression it was checked
on, and the premise in the calculus's own words with its citation. -/
def verdictSection (d : Deriv) : List String :=
  match d.result with
  | some (T, Γ') =>
      ["Verdict: ACCEPTED by the checker (§5).",
       "         type " ++ Print.tyName T ++ ", outgoing Γ;Σ " ++ clip 60 (ctxLine Γ'),
       "",
       "         `check_sound` ties this acceptance to a real derivation, so the §7",
       "         safety theorems apply to this program."]
  | none =>
      "Verdict: REJECTED by the checker (§5)." ::
      "" ::
      (match deepestFailure d with
       | some (r, e, binders, why) =>
           ["The premise that fails, at " ++ r ++ ":", "",
            "    on  " ++ clip 74 (exprLine binders e), ""] ++ para 4 74 why
       | none => ["(no failing premise recorded — this cannot happen)"])

/-- A complete plain-text explanation of one fragment program: its source,
the checker's verdict and derivation (§5), and the machine's run (§6). -/
def render (name description : String) (rules : List String) (e : Expr) : String :=
  let d := explain [] e
  let t := traceEval 0 [] [] [] e
  let lines :=
    [bar '═' 80, " " ++ name, bar '═' 80] ++
    para 0 78 description ++
    ["", "Rules exercised: " ++ String.intercalate " · " rules] ++
    section' "The program" ++
    [sp 4 ++ Print.expr [] 1 e] ++
    section' "What the checker says (§5)" ++
    verdictSection d ++
    section' "The derivation (§5)" ++
    ["Each node is one rule; `e` is the expression it concludes about, `Γ;Σ` the",
     "fused context flowing into it, and `⇒ T ⊣ Σ'` its conclusion. A rule's",
     "premises are the nodes indented under it, in premise order.", ""] ++
    derivLines 2 d ++
    section' "The run (§6)" ++
    ["One row per evaluated node, in execution order: a node's premises run before",
     "the node itself, so the table reads top to bottom as the machine ran. `d<n>`",
     "is the nesting depth and `>>` marks a drop event.", ""] ++
    ((numbered 1 t.steps).map (fun p => stepLines p.1 p.2)).flatten ++
    ["", bar '─' 80] ++
    outcomeLines t.res ++
    [""]
  String.intercalate "\n" lines ++ "\n"

/-- (helper) The rendering of one bridge corpus case (`Corpus.lean`). -/
def renderCase (c : Corpus.Case) : String :=
  render c.name c.description c.rules c.expr

end Text
end Explain
end RueCore
