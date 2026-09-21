import RueCore.Checker
import RueCore.Examples
import RueCore.Print

/-!
# RueCore.Corpus — the bridge corpus (ADR-0097, RUE-2227)

Every case pairs a fragment program with what the mechanization says about
it: the verified checker's verdict (§5, `check_sound`) and the interpreter's
outcome (§6, `eval`). The exporter prints each program as Rue source
(`Print.lean`) and emits the cases as JSON for `crates/rue-oracle-diff`'s
consumer (RUE-2228), which runs the compiler, the oracle, and the native
binary on the source and reports every pairwise disagreement.

## The JSON contract

One array of case objects. Fields:

* `name`, `description`, `rules` — identity, one sentence for a reader, and
  the calculus rules the case exercises.
* `source` — the complete Rue program.
* `verdict` — `{"accept": {"type": <Rue type name>}}` when the checker
  accepts the program (so §7's theorems apply to it and the compiler must
  accept it), or `{"reject": {}}` (the compiler must reject it with an
  ownership diagnostic).
* `expected` — the interpreter's outcome for an accepted program:
  `{"kind": "ok", "stdout": [<line>...], "exit": 0}` where each line is one
  drop event's payload in trace order followed by the program's value
  (`Print.lean` documents the mapping), or `{"kind": "panic", "panic":
  "overflow" | "divZero"}` for a §6.12 trap, where the trace is not
  observable (the machine discards it) and only the trap kind is compared.
  For a rejected program `expected` is `{"kind": "stuck", "violation":
  <name>}`: the refusal the machine reaches, kernel-checked in
  `Examples.lean` and below, which the bridge cannot observe because the
  compiler rejects the program first. A rejected program whose executed path
  never reaches the refusal — it lies on a path the program does not take,
  whether a §5.5 join disagreement or a refusal inside the arm the condition
  skips — carries the `ok` or `panic` outcome of the executed path instead,
  so a compiler that accepts it unsoundly is still compared against what the
  machine does. The seed corpus has no such case; the generator (`Gen.lean`)
  produces them. A `panic` outcome carries no trace on
  the Lean side (`EvalRes.panic` discards it), so drops before a trap are a
  blind spot of the bridge at this fragment; RUE-2282 gives `.panic` its
  trace. Drop lines and the value line are both bare integers, so the
  projection is not injective: a drop of payload `n` swapped with a value
  `n` would not be told apart. Accepted at fragment scope.

The output is deterministic: cases are listed in a fixed order and nothing
depends on the environment.

The declarations here are cases and their export, not rules (`xref: examples`
for `scripts/validate-lean-xref-index.py`, which indexes a case's citations
without requiring them).
-/

namespace RueCore.Corpus

open Expr

/-- A corpus case: a closed fragment program with its documentation. -/
structure Case where
  name : String
  description : String
  rules : List String
  expr : Expr

/-! ## The seed corpus

`Examples.lean`'s programs, plus one witness per §7 bullet the fragment
covers and one per drop point of the machine. -/

def cases : List Case := [
  { name := "scalars",
    description := "Well-typed scalar flow: a binding used twice by copy.",
    rules := ["(Use-Copy) §5.1", "(Let) §5.3"],
    expr := Examples.scalars },
  { name := "affine_scope_drop",
    description := "An affine resource silently dropped at scope exit; the trace shows the drop before the value.",
    rules := ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"],
    expr := Examples.affineDrop },
  { name := "linear_consumed",
    description := "A linear resource consumed exactly once; no drop event.",
    rules := ["(Use-Move) §5.1", "consume §5.1"],
    expr := Examples.linearConsumed },
  { name := "linear_leaked",
    description := "A linear resource reaching scope exit unconsumed: rejected statically (E0406) and refused dynamically (linearLeak).",
    rules := ["§5.6 residual-linear leak check", "3.8:32"],
    expr := Examples.linearLeaked },
  { name := "use_after_move",
    description := "A moved affine binding used again: rejected statically (E0205) and refused dynamically (useAfterMove).",
    rules := ["(Use-Move) §5.1", "3.8:5"],
    expr := Examples.useAfterMove },
  { name := "reinit",
    description := "Move a linear value out, assign a new one back in, consume it: legal reinitialization.",
    rules := ["(Assign) §5.2", "3.8:55"],
    expr := Examples.reinit },
  { name := "linear_half_consumed",
    description := "A linear value consumed in one arm of an if only: the §5.5 join rejects it; dynamically it leaks on the other path.",
    rules := ["(If) §5.5 join", "3.8:50"],
    expr := Examples.linearHalfConsumed },
  { name := "overflow",
    description := "intMax + 1 traps with a defined overflow panic.",
    rules := ["§6.4 arithmetic traps", "3.1:6"],
    expr := Examples.overflow },
  { name := "div_zero",
    description := "Division by zero traps with a defined panic.",
    rules := ["§6.4 arithmetic traps", "4.2:11"],
    expr := Examples.divZero },
  { name := "copy_resource",
    description := "A copy resource: @drop is a no-op, uses copy, nothing is ever printed for it.",
    rules := ["(Use-Copy) §5.1", "(@Drop-Copy) §5.3"],
    expr := letIn false (mkres .copy (intLit 5))
      (seq (drop 0) (add (consume (use 0)) (consume (use 0)))) },
  { name := "affine_explicit_drop",
    description := "An affine resource dropped explicitly with @drop: one drop line, at the @drop site, nothing at scope exit.",
    rules := ["(@Drop) §5.3", "§6.11", "3.9:37"],
    expr := letIn false (mkres .affine (intLit 8)) (seq (drop 0) (intLit 2)) },
  { name := "linear_explicit_drop",
    description := "A linear resource discharged by the core's @drop (the only non-move discharge of a linear obligation); printed as a consuming read, since RLinear cannot carry a destructor (Print.lean).",
    rules := ["(@Drop) §5.3", "3.9:39"],
    expr := letIn false (mkres .linear (intLit 9)) (seq (drop 0) (intLit 3)) },
  { name := "affine_temporary_discarded",
    description := "An affine value produced and discarded by a sequence: the machine drops the temporary at the end of the statement.",
    rules := ["(Seq) §5.3", "§6.7 temporary drop"],
    expr := seq (mkres .affine (intLit 3)) (intLit 4) },
  { name := "linear_temporary_discarded",
    description := "A linear value produced and discarded by a sequence: rejected statically (3.8:64) and refused dynamically (linearDiscard).",
    rules := ["(Seq) §5.3", "3.8:64"],
    expr := seq (mkres .linear (intLit 3)) (intLit 4) },
  { name := "affine_overwrite",
    description := "Assigning over a live affine value drops the old value at the assignment, then the new one at scope exit.",
    rules := ["(Assign) §5.2", "§6.8 overwrite-drop", "3.9:18"],
    expr := letIn true (mkres .affine (intLit 1))
      (seq (assign 0 (mkres .affine (intLit 2))) (intLit 9)) },
  { name := "linear_overwrite",
    description := "Assigning over a live linear value: rejected statically (3.8:77, the RUE-387 premise) and refused dynamically (linearOverwrite).",
    rules := ["(Assign) §5.2", "3.8:77"],
    expr := letIn true (mkres .linear (intLit 1))
      (seq (assign 0 (mkres .linear (intLit 2))) (consume (use 0))) },
  { name := "join_agrees",
    description := "A linear value consumed in both arms of an if: the join agrees, the program is accepted, and the value chosen is the taken arm's.",
    rules := ["(If) §5.5 join", "3.8:50"],
    expr := letIn false (mkres .linear (intLit 6))
      (ite (lt (intLit 1) (intLit 2)) (consume (use 0)) (add (consume (use 0)) (intLit 1))) },
  { name := "nested_scopes",
    description := "Two affine bindings in nested scopes drop innermost first, each at its own scope's close.",
    rules := ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7", "3.9:2"],
    expr := letIn false (mkres .affine (intLit 1))
      (letIn false (mkres .affine (intLit 2)) (intLit 0)) },
  { name := "resource_result",
    description := "The program's value is a resource: main observes its payload and never drops it.",
    rules := ["§4.3 expression value"],
    expr := letIn false (intLit 4) (mkres .affine (use 0)) },
  { name := "cond_drop_affine",
    description := "An affine resource dropped explicitly in one arm of an if and left to scope exit on the other: accepted (the join sends it to MovedOut), one drop line either way. The bridge found the compiler ICEing on this (RUE-2290, fixed); the case stays as the regression signal.",
    rules := ["(@Drop) §5.3", "(If) §5.5 join", "3.9:38"],
    expr := letIn false (mkres .affine (intLit 5))
      (seq (ite (boolLit true) (drop 0) unitLit) (intLit 9)) },
  { name := "bool_result",
    description := "A boolean value from a comparison.",
    rules := ["§5.8 operator statics", "§6.4"],
    expr := lt (intLit 3) (intLit 2) }
]

/-! ## Witnesses for the refusals no `Examples.lean` program reaches -/

example : eval [] [] (letIn true (mkres .linear (intLit 1))
    (seq (assign 0 (mkres .linear (intLit 2))) (consume (use 0))))
    = .stuck .linearOverwrite := by rfl
example : eval [] [] (seq (mkres .linear (intLit 3)) (intLit 4))
    = .stuck .linearDiscard := by rfl
example : check [] (seq (mkres .linear (intLit 3)) (intLit 4)) = none := by rfl

/-! ## Outcomes, from the mechanization -/

/-- The printed line for a value the machine observes: a payload or a
scalar (`Print.observeValue`). -/
def valueLine : Val → Option String
  | .int n => some (toString n)
  | .bool b => some (if b then "true" else "false")
  | .unit => none
  | .res _ n => some (toString n)

/-- One stdout line per drop event: the dropped resource's payload. `eval`
emits events only for affine and linear resources, so any other value here
is a broken invariant, reported loudly rather than printed as an empty
line. -/
def eventLine : Event → String
  | .drop _ (.res _ n) => toString n
  | .dropTemp (.res _ n) => toString n
  | ev => panic! s!"drop event of a non-resource value: {repr ev}"

def panicName : PanicKind → String
  | .overflow => "overflow"
  | .divZero => "divZero"

def violationName : Violation → String
  | .useAfterMove => "useAfterMove"
  | .useAfterDrop => "useAfterDrop"
  | .linearLeak => "linearLeak"
  | .linearOverwrite => "linearOverwrite"
  | .linearDiscard => "linearDiscard"
  | .unbound => "unbound"
  | .typeConfusion => "typeConfusion"

/-- A one-line reading of the outcome, for the program's header comment. -/
def outcomeSummary (c : Case) : String :=
  match check [] c.expr, eval [] [] c.expr with
  | none, .stuck w => "rejected by the checker; the machine would refuse with " ++ violationName w
  | none, .ok _ v tr =>
      let lines := tr.map eventLine ++ (valueLine v).toList
      "rejected by the checker; the refusal lies on a path not taken, and the executed path prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | none, .panic k => "rejected by the checker; the refusal lies on a path not taken, and the executed path traps with " ++ panicName k
  | some _, .ok _ v tr =>
      let lines := tr.map eventLine ++ (valueLine v).toList
      "accepted; prints " ++ (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | some _, .panic k => "accepted; traps with " ++ panicName k
  | some _, .stuck w => "accepted yet refused with " ++ violationName w ++ " (impossible by soundness)"

/-! ## JSON -/

def jsonString (s : String) : String :=
  let escaped := s.foldl (fun acc c =>
    acc ++ match c with
      | '"' => "\\\""
      | '\\' => "\\\\"
      | '\n' => "\\n"
      | '\t' => "\\t"
      | '\r' => "\\r"
      | c => String.singleton c) ""
  "\"" ++ escaped ++ "\""

def jsonArray (items : List String) : String :=
  "[" ++ String.intercalate ", " items ++ "]"

def verdictJson (c : Case) : String :=
  match check [] c.expr with
  | some (T, _) => "{\"accept\": {\"type\": " ++ jsonString (Print.tyName T) ++ "}}"
  | none => "{\"reject\": {}}"

def expectedJson (c : Case) : String :=
  match eval [] [] c.expr with
  | .ok _ v tr =>
      let lines := tr.map eventLine ++ (valueLine v).toList
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray (lines.map jsonString) ++ ", \"exit\": 0}"
  | .panic k => "{\"kind\": \"panic\", \"panic\": " ++ jsonString (panicName k) ++ "}"
  | .stuck w => "{\"kind\": \"stuck\", \"violation\": " ++ jsonString (violationName w) ++ "}"

def caseJson (c : Case) : String :=
  "  {\n" ++
  "    \"name\": " ++ jsonString c.name ++ ",\n" ++
  "    \"description\": " ++ jsonString c.description ++ ",\n" ++
  "    \"rules\": " ++ jsonArray (c.rules.map jsonString) ++ ",\n" ++
  "    \"source\": " ++ jsonString (Print.program c.name c.description c.rules (outcomeSummary c) c.expr) ++ ",\n" ++
  "    \"verdict\": " ++ verdictJson c ++ ",\n" ++
  "    \"expected\": " ++ expectedJson c ++ "\n" ++
  "  }"

/-- A list of cases as one JSON document. -/
def jsonOf (cs : List Case) : String :=
  "[\n" ++ String.intercalate ",\n" (cs.map caseJson) ++ "\n]\n"

/-- The seed corpus as one JSON document. -/
def json : String := jsonOf cases

end RueCore.Corpus
