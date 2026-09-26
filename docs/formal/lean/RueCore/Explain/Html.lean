import RueCore.Explain.Ledger

/-!
# RueCore.Explain.Html — the self-contained page (RUE-2246)

One HTML file per program: no scripts, no external assets, inline CSS, so a
page opens from a file and reads as a document. The shape is the text
rendering's: the program, the checker's verdict with the failing premise
first when it rejects, the §5 derivation as a nested list carrying `Γ;Σ` at
every node, and the §6 run as a table that reads top to bottom, each row
showing the store before and after as a small table of its own and marking
the drop events.

As in the text renderer, the verdict is `Deriv.result` and the outcome is
`Trace.res`, which `explain_result` and `traceEval_res` tie to `check` and
`eval`.
-/

namespace RueCore
namespace Explain
namespace Html

/-- (helper) Escape the five characters that cannot appear literally in
HTML text or in an attribute value. -/
def esc (s : String) : String :=
  s.foldl (fun acc c =>
    acc ++ match c with
      | '&' => "&amp;"
      | '<' => "&lt;"
      | '>' => "&gt;"
      | '"' => "&quot;"
      | '\'' => "&#39;"
      | c => String.singleton c) ""

/-- (helper) An element with no attributes. -/
def tag (name body : String) : String := "<" ++ name ++ ">" ++ body ++ "</" ++ name ++ ">"

/-- (helper) An element with a class. -/
def tagc (name cls body : String) : String :=
  "<" ++ name ++ " class=\"" ++ cls ++ "\">" ++ body ++ "</" ++ name ++ ">"

/-- (helper) The page's stylesheet. Inline, small, and light/dark aware, so
one file is the whole artifact. -/
def style : String :=
  ":root { --bg: #fdfdfb; --fg: #1b1b1a; --muted: #6a6a64; --line: #d8d6cf;\n" ++
  "        --accept: #1c6b3c; --reject: #a2261f; --drop: #8a4b00; --panel: #f4f3ee; }\n" ++
  "@media (prefers-color-scheme: dark) {\n" ++
  "  :root { --bg: #17181a; --fg: #e8e6e1; --muted: #9b9a93; --line: #35373a;\n" ++
  "          --accept: #7fca9b; --reject: #f0918a; --drop: #e0aa62; --panel: #1f2124; }\n" ++
  "}\n" ++
  "* { box-sizing: border-box; }\n" ++
  "body { margin: 0 auto; max-width: 62rem; padding: 2rem 1rem 4rem;\n" ++
  "       background: var(--bg); color: var(--fg);\n" ++
  "       font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif; }\n" ++
  "h1 { font-size: 1.6rem; margin: 0 0 .25rem; }\n" ++
  "h2 { font-size: 1.1rem; margin: 2.2rem 0 .6rem; padding-bottom: .3rem;\n" ++
  "     border-bottom: 1px solid var(--line); }\n" ++
  "p.lead { color: var(--muted); margin: .2rem 0 .8rem; }\n" ++
  "code, pre, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }\n" ++
  "pre.program { background: var(--panel); border: 1px solid var(--line); border-radius: 6px;\n" ++
  "              padding: .9rem 1rem; overflow-x: auto; font-size: 13px; }\n" ++
  ".rules span { display: inline-block; background: var(--panel); border: 1px solid var(--line);\n" ++
  "              border-radius: 999px; padding: .05rem .6rem; margin: 0 .3rem .3rem 0;\n" ++
  "              font-size: 12px; color: var(--muted); }\n" ++
  ".verdict { border-left: 4px solid var(--accept); background: var(--panel);\n" ++
  "           padding: .7rem 1rem; border-radius: 0 6px 6px 0; }\n" ++
  ".verdict.reject { border-left-color: var(--reject); }\n" ++
  ".verdict .label { font-weight: 700; color: var(--accept); }\n" ++
  ".verdict.reject .label { color: var(--reject); }\n" ++
  ".premise { margin-top: .6rem; }\n" ++
  ".premise .where { color: var(--muted); font-size: 13px; }\n" ++
  "ul.deriv, ul.deriv ul { list-style: none; margin: 0; padding-left: 1.1rem; }\n" ++
  "ul.deriv li { border-left: 1px solid var(--line); padding: .35rem 0 .1rem .8rem;\n" ++
  "                margin: .15rem 0; }\n" ++
  ".rule { font-weight: 600; }\n" ++
  ".node .expr { font-size: 13px; }\n" ++
  ".node .ctx { font-size: 12px; color: var(--muted); }\n" ++
  ".concl { font-size: 13px; color: var(--accept); }\n" ++
  ".failed { font-size: 13px; color: var(--reject); }\n" ++
  "table.trace { border-collapse: collapse; width: 100%; font-size: 13px; }\n" ++
  "table.trace th { text-align: left; border-bottom: 1px solid var(--line);\n" ++
  "                 padding: .3rem .5rem; color: var(--muted); font-weight: 600; }\n" ++
  "table.trace td { border-bottom: 1px solid var(--line); padding: .35rem .5rem;\n" ++
  "                 vertical-align: top; }\n" ++
  "table.trace tr.has-drop { background: rgba(190, 120, 20, .13); }\n" ++
  "table.store { border-collapse: collapse; font-size: 12px; }\n" ++
  "table.store td { padding: 0 .4rem 0 0; border: 0; white-space: nowrap; }\n" ++
  "table.store td.loc { color: var(--muted); }\n" ++
  ".events { color: var(--drop); font-weight: 600; }\n" ++
  ".none { color: var(--muted); }\n" ++
  ".ok { color: var(--accept); }\n" ++
  ".bad { color: var(--reject); }\n" ++
  "footer { margin-top: 3rem; color: var(--muted); font-size: 12px; }\n" ++
  "a { color: inherit; }\n"

/-- (helper) A store as a small two-column table (location, contents) that
reads top to bottom, oldest allocation first. -/
def storeTable (H : Store) : String :=
  if H.isEmpty then tagc "span" "none" "(empty)"
  else
    "<table class=\"store\">" ++
      String.intercalate "" ((storeRows 0 H).map (fun r =>
        "<tr><td class=\"loc\">" ++ esc r.1 ++ "</td><td>" ++ esc r.2 ++ "</td></tr>")) ++
    "</table>"

/-- (helper) A fused `Γ;Σ` context as a comma-separated list of entries. -/
def ctxHtml (Γ : Ctx) : String :=
  if Γ.isEmpty then tagc "span" "none" "(empty)" else esc (ctxLine Γ)

/-- One node of the §5 derivation: the rule, the expression, the incoming
`Γ;Σ`, the conclusion `⇒ T ⊣ Σ'` or the premise that failed, and the
premises as a nested list. -/
partial def derivHtml (P : Program) (R : Ty) : Deriv → String
  | .node r Γ e v kids =>
      let binders := binderTys Γ
      let concl := match v with
        | .accept c Ω =>
            tagc "div" "concl" ("⇒ " ++ esc (cTyName c) ++ " ⊣ " ++
              (match Ω.norm, Ω.brk with
               | some Γ', [] => ctxHtml Γ'
               | _, _ => esc (outLine Ω)))
        | .reject why => tagc "div" "failed" ("✗ " ++ esc why)
      let children :=
        if kids.isEmpty then ""
        else "<ul>" ++
          String.intercalate "" (kids.map (fun k => tag "li" (derivHtml P R k))) ++ "</ul>"
      tagc "div" "node"
        (tagc "div" "rule" (esc r) ++
         tagc "div" "expr mono" (esc (exprLine P R binders e)) ++
         tagc "div" "ctx mono" ("Γ;Σ " ++ ctxHtml Γ) ++
         concl) ++ children

/-- (helper) What one step produced. -/
def stepResHtml : StepRes → String
  | .value v => tagc "span" "ok" (esc ("value " ++ valLine v))
  | .unwound v =>
      tagc "span" "ok" (esc ("value " ++ valLine v)) ++
        tagc "div" "ctx" "handed past this node by a `return` (§6.9)"
  | .breaking =>
      tagc "span" "ok" "break" ++ tagc "div" "ctx" "unwinding to the enclosing loop (§6.10)"
  | .panicked k => tagc "span" "bad" (esc ("panic: " ++ Corpus.panicName k))
  | .refuse w why =>
      tagc "span" "bad" (esc ("refused: " ++ Corpus.violationName w)) ++
        tagc "div" "ctx" (esc why)
  | .exhausted =>
      tagc "span" "bad" "out of fuel" ++
        tagc "div" "ctx" "the interpreter stopped early; this is not a machine state"

/-- One row of the §6 step table. A row whose node emitted a drop event is
marked, so the drop points of a run stand out. -/
def stepRow (P : Program) (n : Nat) (s : Step) : String :=
  let cls := if s.events.isEmpty then "" else " class=\"has-drop\""
  "<tr" ++ cls ++ ">" ++
  tag "td" (toString n) ++
  tag "td" (tagc "div" "rule" (esc s.rule) ++
            tagc "div" "expr mono"
              ("<span style=\"opacity:.45\">" ++ esc (bar' s.depth) ++ "</span>" ++
               esc (s.text P))) ++
  tag "td" (storeTable s.storeBefore) ++
  tag "td" (storeTable s.storeAfter) ++
  tag "td" (if s.events.isEmpty then tagc "span" "none" "—"
            else tagc "span" "events" (esc (eventsLine s.events))) ++
  tag "td" (stepResHtml s.res) ++
  "</tr>"
where
  /-- (helper) A depth marker, so nesting is visible in the table. -/
  bar' (d : Nat) : String := "".pushn '·' (2 * min d 8)

/-- The checker's verdict panel (§5): accepted with its type, or rejected
with the failing premise named prominently — the rule it belongs to, the
subexpression it was checked on, and the premise in the calculus's own words
with its citation. -/
def verdictHtml (P : Program) (ds : List (Nat × FnDef × Deriv)) : String :=
  let failing :=
    (ds.map (fun t => (deepestFailure t.2.2).map (fun f => (t.1, t.2.1, f)))).reduceOption
  if checkProgram P then
    tagc "div" "verdict"
      (tagc "div" "label" "ACCEPTED by the checker (§5)" ++
       tagc "div" ""
         ("Every function is well-formed by (Fn) §5.8 and the entry point takes no " ++
          "parameters.") ++
       tagc "div" "ctx"
         ("checkProgram_sound ties this acceptance to a real derivation, so the §7 " ++
          "safety theorems apply to this program."))
  else
    tagc "div" "verdict reject"
      (tagc "div" "label" "REJECTED by the checker (§5)" ++
       match failing.head? with
       | some (i, fd, (r, e, binders, why)) =>
           tagc "div" "premise"
             (tagc "div" "where" ("The premise that fails, in " ++ esc (fnHeader i fd) ++
                ", at " ++ esc r ++ ", on " ++
                tagc "code" "mono" (esc (exprLine P fd.ret binders e)) ++ ":") ++
              tag "div" (esc why))
       | none =>
           tagc "div" "premise"
             (tag "div"
               (esc (if !checkDecls P.decls then
                 "No function body's derivation failed, so the rejection is checkDecls's " ++
                 "own check of the declarations (§3, before any function is checked): a " ++
                 "struct or enum declaration violates 3.8:18/3.9:31 (a @copy declaration " ++
                 "may declare no destructor) or 3.9:44 (no destructor-bearing declaration " ++
                 "may carry a linear field), or a declaration contains itself by value, " ++
                 "directly or through a cycle (3.0:5, E0483)."
               else
                 "No function body's derivation failed, so the rejection is the " ++
                 "whole-program premise: a by-value parameter or a body-local binding " ++
                 "is still Owned at a linear type where its function's body ends " ++
                 "((Fn) §5.8's second clause, 3.8:62), or the entry point does not " ++
                 "take an empty parameter list."))))

/-- The §6 machine's outcome, in one line: a value with its drop trace, a
defined trap (§6.12), or a refusal. -/
def outcomeHtml : EvalRes → String
  | .ok _ v tr =>
      tagc "span" "ok" (esc ("ok — value " ++ valLine v)) ++
      tagc "div" "ctx" ("drop trace: " ++
        (if tr.isEmpty then "(no drops)" else esc (eventsLine tr)))
  | .returned _ v tr =>
      tagc "span" "ok" (esc ("ok — value " ++ valLine v ++ " (handed back by a `return`)")) ++
      tagc "div" "ctx" ("drop trace: " ++
        (if tr.isEmpty then "(no drops)" else esc (eventsLine tr)))
  | .broke _ _ _ =>
      tagc "span" "bad" "a `break` outside every loop" ++
      tagc "div" "ctx" "impossible: the call boundary refuses it (§6.10)"
  | .outOfFuel =>
      tagc "span" "bad" "out of fuel" ++
      tagc "div" "ctx"
        ("the interpreter stopped before the program did; fuel_mono says a larger " ++
         "bound never changes an answer, so this is a bound too small, not a claim " ++
         "about the program")
  | .panic k tr =>
      tagc "span" "bad" (esc ("panic: " ++ Corpus.panicName k)) ++
      tagc "div" "ctx" (esc ("a defined trap (§6.12), not a violation; the observable " ++
        "output up to the trap is " ++ (if tr.isEmpty then "empty" else eventsLine tr)))
  | .stuck w =>
      tagc "span" "bad" (esc ("refused: " ++ Corpus.violationName w)) ++
      tagc "div" "ctx" (esc (violationPremise w))

/-- (helper) The document frame. -/
def page (title body : String) : String :=
  "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n" ++
  "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n" ++
  "<title>" ++ esc title ++ "</title>\n<style>\n" ++ style ++ "</style>\n</head>\n<body>\n" ++
  body ++
  "<footer>Rendered from the Lean mechanization of the Rue core calculus " ++
  "(docs/formal/lean, <code>lake exe ruecore-explain</code>). The verdict is the " ++
  "checker's (proved sound) and the run is the interpreter's: " ++
  "<code>explain_result</code> and <code>traceEval_res</code> prove it.</footer>\n" ++
  "</body>\n</html>\n"

/-- The identity ledger (`Explain.Ledger`) as a table: one row per owned
identity, with the step that minted it, the steps that ended it, the steps
whose destructor ran on it, and whether it was ended exactly once (§7). -/
def ledgerHtml (P : Program) (rows : List (Nat × Step)) (res : EvalRes) : String :=
  let es := Ledger.entries P.decls rows
  let refs := fun (ns : List Nat) =>
    if ns.isEmpty then "—" else String.intercalate ", " (ns.map Ledger.rowRef)
  tagc "p" "lead"
    ("One row per owned identity: the step that minted it, the steps that ended it " ++
     "(a drop marker, a discarded temporary, a consumption), and the steps whose " ++
     "destructor ran on it. <code>drop_exactly_once</code> is one entry in the ended " ++
     "column per identity. Read in row order, and [n.k] (the k-th end of row n) " ++
     "inside a row, the ends are the trace's order, which <code>drop_order</code> " ++
     "fixes: a value's destructors inside its drop in §6.11's order, and cells " ++
     "last-in first-out.") ++
  (if es.isEmpty then "" else
    "<table class=\"trace\"><thead><tr><th>identity</th><th>minted</th><th>ended</th>" ++
    "<th>destructor</th><th>verdict</th></tr></thead><tbody>" ++
    String.intercalate "" (es.map (fun e =>
      tag "tr" (tagc "td" "mono" ("#" ++ toString e.id) ++
        tag "td" (refs e.minted.toList) ++
        tag "td" (esc (Ledger.endsText e)) ++
        tag "td" (refs e.dtors) ++
        tag "td" (let v := Ledger.verdict P.decls res e
                  if v == "once" then tagc "span" "ok" v else tagc "span" "bad" (esc v))))) ++
    "</tbody></table>") ++
  tag "p" (esc (Ledger.summary P.decls res es))

/-- A complete, self-contained page explaining one fragment program: its
§5 derivation and its §6 run. -/
def render (name description : String) (rules : List String) (P : Program) : String :=
  let ds := programDerivs P 0 P.fns
  let t := runTrace Corpus.exportOps P Corpus.exportFuel
  page name
    (tag "h1" (esc name) ++
     tagc "p" "lead" (esc description) ++
     tagc "p" "rules" (String.intercalate "" (rules.map (fun r => tag "span" (esc r)))) ++
     tag "h2" "The program" ++
     tagc "pre" "program" (esc (Print.moduleItems P ++ Print.fnItems P 0 P.fns)) ++
     tag "h2" "What the checker says (§5)" ++
     verdictHtml P ds ++
     tag "h2" "The derivations (§5)" ++
     tagc "p" "lead"
       ("One tree per function body, checked from (Fn) §5.8's entry context. Each node " ++
        "is one rule of §5: the expression it concludes about, the fused Γ;Σ flowing " ++
        "into it, and its conclusion. A rule's premises are nested under it.") ++
     String.intercalate ""
       (ds.map (fun p =>
         tag "h3" (esc (fnHeader p.1 p.2.1)) ++
         "<ul class=\"deriv\">" ++ tag "li" (derivHtml P p.2.1.ret p.2.2) ++ "</ul>")) ++
     tag "h2" "The run (§6)" ++
     tagc "p" "lead"
       ("One row per evaluated node, in execution order — a node's premises run before " ++
        "the node itself, so the table reads top to bottom as the machine ran. A " ++
        "callee's rows are nested deeper than its call's. Rows that drop something are " ++
        "highlighted. #n after an aggregate is its identity, minted when it was built; " ++
        "a † cell is a retired binding or an identity's reserved slot.") ++
     "<table class=\"trace\"><thead><tr><th>#</th><th>node</th><th>store before</th>" ++
     "<th>store after</th><th>drop events</th><th>result</th></tr></thead><tbody>" ++
     String.intercalate "" ((numbered 1 t.steps).map (fun p => stepRow P p.1 p.2)) ++
     "</tbody></table>" ++
     tag "h2" "Identities (§7)" ++
     ledgerHtml P (numbered 1 t.steps) t.res ++
     tag "h2" "Outcome" ++
     tag "p" (outcomeHtml t.res))

/-- (helper) The page for one bridge corpus case (`Corpus.lean`). -/
def renderCase (c : Corpus.Case) : String :=
  render c.name c.description c.rules c.prog

/-- (helper) One row of the index: the case, its checker verdict, and the
machine's outcome in words. -/
def indexRow (c : Corpus.Case) : String :=
  let verdict :=
    if checkProgram c.prog then
      tagc "span" "ok" ("accepted · " ++ esc (Corpus.resultTyName c))
    else tagc "span" "bad" "rejected"
  "<tr>" ++
  tag "td" ("<a href=\"" ++ esc c.name ++ ".html\">" ++ esc c.name ++ "</a>") ++
  tag "td" verdict ++
  tag "td" (esc (Corpus.outcomeSummary c)) ++
  tag "td" (tagc "span" "ctx" (esc c.description)) ++
  "</tr>"

/-- (helper) The index page: every corpus case with its verdict and
outcome, linking to its own page. -/
def index (cases : List Corpus.Case) : String :=
  page "RueCore — explained programs"
    (tag "h1" "RueCore — explained programs" ++
     tagc "p" "lead"
       ("Every case of the bridge corpus, explained: the §5 derivation the " ++
        "checker builds and the §6 run the interpreter performs. Generated by " ++
        "lake exe ruecore-explain --html.") ++
     "<table class=\"trace\"><thead><tr><th>case</th><th>checker (§5)</th>" ++
     "<th>machine (§6)</th><th>what it shows</th></tr></thead><tbody>" ++
     String.intercalate "" (cases.map indexRow) ++
     "</tbody></table>")

end Html
end Explain
end RueCore
