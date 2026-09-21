import RueCore.Checker
import RueCore.Examples
import RueCore.Print

/-!
# RueCore.Corpus — the bridge corpus (ADR-0097, RUE-2227)

Every case pairs a fragment program with what the mechanization says about
it: the verified checker's verdict (§5, `checkProgram_sound`) and the
interpreter's outcome (§6, `run`). The exporter prints each program as Rue
source
(`Print.lean`) and emits the cases as JSON for `crates/rue-oracle-diff`'s
consumer (RUE-2228), which runs the compiler, the oracle, and the native
binary on the source and reports every pairwise disagreement.

## The JSON contract

One array of case objects. Fields:

* `name`, `description`, `rules` — identity, one sentence for a reader, and
  the calculus rules the case exercises.
* `source` — the complete Rue program.
* `verdict` — `{"accept": {"type": <Rue type name>}}` when the checker
  accepts the program — every function well-formed by (Fn) §5.8 and the
  entry point taking no parameters (`checkProgram`) — so §7's theorems apply
  to it and the compiler must accept it; or `{"reject": {}}` (the compiler
  must reject it with an ownership diagnostic). The type is the entry
  function's declared return type.

  An `accept` verdict is backed by a proof (`checkProgram_sound` plus §7), so
  a compiler that rejects one is wrong. A `reject` verdict is **not**: it is
  the absence of an acceptance from an algorithm that is deliberately
  narrower than the rule, so it is only trustworthy on shapes where `check`
  is complete. It is not complete on `return`: a `return` arm of an `if`
  contributes its post-operand state to §5.5's join, where §5.7 excludes a
  diverging arm's state entirely, so a binding that arm moved out is
  unusable after the `if` and a program the calculus derives — and the
  compiler accepts — is rejected here (`Checker.lean`, "what completeness
  costs"). That shape would be a *false* bridge failure, so nothing produces
  it: the seed cases below use `return` only where `check` is complete, and
  `Gen.lean` emits no `ret` at all.
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

Every case is evaluated at one fixed bound, `exportFuel`, and a case the
bound does not complete is **not exported**: `outOfFuel` is the interpreter
admitting it stopped early, not a claim about the program, and there is no
honest `expected` to compare an implementation against. `fuel_mono`
(`Soundness.lean`) is why one bound is enough to speak for every larger one.
The seed corpus completes far inside the bound; a generated program that did
not would simply be absent from the output.

The output is deterministic: cases are listed in a fixed order and nothing
depends on the environment.

The declarations here are cases and their export, not rules (`xref: examples`
for `scripts/validate-lean-xref-index.py`, which indexes a case's citations
without requiring them).
-/

namespace RueCore.Corpus

open Expr

/-- A corpus case: a closed fragment program with its documentation. A
program is a list of function definitions, entered at index `0` (§6.12). -/
structure Case where
  name : String
  description : String
  rules : List String
  prog : Program

/-- The fuel every exported case is evaluated at. Deep enough for every seed
case by a wide margin; a case the bound does not complete is left out of the
export rather than given an outcome (module docstring). -/
def exportFuel : Nat := 100000

/-! ## The seed corpus

`Examples.lean`'s programs, plus one witness per §7 bullet the fragment
covers and one per drop point of the machine. -/

def cases : List Case := [
  { name := "scalars",
    description := "Well-typed scalar flow: a binding used twice by copy.",
    rules := ["(Use-Copy) §5.1", "(Let) §5.3"],
    prog := Program.entry .int <| Examples.scalars
    },
  { name := "affine_scope_drop",
    description := "An affine resource silently dropped at scope exit; the trace shows the drop before the value.",
    rules := ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7"],
    prog := Program.entry .int <| Examples.affineDrop
    },
  { name := "linear_consumed",
    description := "A linear resource consumed exactly once; no drop event.",
    rules := ["(Use-Move) §5.1", "consume §5.1"],
    prog := Program.entry .int <| Examples.linearConsumed
    },
  { name := "linear_leaked",
    description := "A linear resource reaching scope exit unconsumed: rejected statically (E0406) and refused dynamically (linearLeak).",
    rules := ["§5.6 residual-linear leak check", "3.8:32"],
    prog := Program.entry .int <| Examples.linearLeaked
    },
  { name := "use_after_move",
    description := "A moved affine binding used again: rejected statically (E0205) and refused dynamically (useAfterMove).",
    rules := ["(Use-Move) §5.1", "3.8:5"],
    prog := Program.entry .int <| Examples.useAfterMove
    },
  { name := "reinit",
    description := "Move a linear value out, assign a new one back in, consume it: legal reinitialization.",
    rules := ["(Assign) §5.2", "3.8:55"],
    prog := Program.entry .int <| Examples.reinit
    },
  { name := "linear_half_consumed",
    description := "A linear value consumed in one arm of an if only: the §5.5 join rejects it; dynamically it leaks on the other path.",
    rules := ["(If) §5.5 join", "3.8:50"],
    prog := Program.entry .int <| Examples.linearHalfConsumed
    },
  { name := "overflow",
    description := "intMax + 1 traps with a defined overflow panic.",
    rules := ["§6.4 arithmetic traps", "3.1:6"],
    prog := Program.entry .int <| Examples.overflow
    },
  { name := "div_zero",
    description := "Division by zero traps with a defined panic.",
    rules := ["§6.4 arithmetic traps", "4.2:11"],
    prog := Program.entry .int <| Examples.divZero
    },
  { name := "copy_resource",
    description := "A copy resource: @drop is a no-op, uses copy, nothing is ever printed for it.",
    rules := ["(Use-Copy) §5.1", "(@Drop-Copy) §5.3"],
    prog := Program.entry .int <| letIn false (mkres .copy (intLit 5))
      (seq (drop 0) (add (consume (use 0)) (consume (use 0))))
    },
  { name := "affine_explicit_drop",
    description := "An affine resource dropped explicitly with @drop: one drop line, at the @drop site, nothing at scope exit.",
    rules := ["(@Drop) §5.3", "§6.11", "3.9:37"],
    prog := Program.entry .int <| letIn false (mkres .affine (intLit 8)) (seq (drop 0) (intLit 2))
    },
  { name := "linear_explicit_drop",
    description := "A linear resource discharged by the core's @drop (the only non-move discharge of a linear obligation); printed as a consuming read, since RLinear cannot carry a destructor (Print.lean).",
    rules := ["(@Drop) §5.3", "3.9:39"],
    prog := Program.entry .int <| letIn false (mkres .linear (intLit 9)) (seq (drop 0) (intLit 3))
    },
  { name := "affine_temporary_discarded",
    description := "An affine value produced and discarded by a sequence: the machine drops the temporary at the end of the statement.",
    rules := ["(Seq) §5.3", "§6.7 temporary drop"],
    prog := Program.entry .int <| seq (mkres .affine (intLit 3)) (intLit 4)
    },
  { name := "linear_temporary_discarded",
    description := "A linear value produced and discarded by a sequence: rejected statically (3.8:64) and refused dynamically (linearDiscard).",
    rules := ["(Seq) §5.3", "3.8:64"],
    prog := Program.entry .int <| seq (mkres .linear (intLit 3)) (intLit 4)
    },
  { name := "affine_overwrite",
    description := "Assigning over a live affine value drops the old value at the assignment, then the new one at scope exit.",
    rules := ["(Assign) §5.2", "§6.8 overwrite-drop", "3.9:18"],
    prog := Program.entry .int <| letIn true (mkres .affine (intLit 1))
      (seq (assign 0 (mkres .affine (intLit 2))) (intLit 9))
    },
  { name := "linear_overwrite",
    description := "Assigning over a live linear value: rejected statically (3.8:77, the RUE-387 premise) and refused dynamically (linearOverwrite).",
    rules := ["(Assign) §5.2", "3.8:77"],
    prog := Program.entry .int <| letIn true (mkres .linear (intLit 1))
      (seq (assign 0 (mkres .linear (intLit 2))) (consume (use 0)))
    },
  { name := "join_agrees",
    description := "A linear value consumed in both arms of an if: the join agrees, the program is accepted, and the value chosen is the taken arm's.",
    rules := ["(If) §5.5 join", "3.8:50"],
    prog := Program.entry .int <| letIn false (mkres .linear (intLit 6))
      (ite (lt (intLit 1) (intLit 2)) (consume (use 0)) (add (consume (use 0)) (intLit 1)))
    },
  { name := "nested_scopes",
    description := "Two affine bindings in nested scopes drop innermost first, each at its own scope's close.",
    rules := ["(Let) §5.3", "§5.6 scope exit", "(D-EndScope) §6.7", "3.9:2"],
    prog := Program.entry .int <| letIn false (mkres .affine (intLit 1))
      (letIn false (mkres .affine (intLit 2)) (intLit 0))
    },
  { name := "resource_result",
    description := "The program's value is a resource: main observes its payload and never drops it.",
    rules := ["§4.3 expression value"],
    prog := Program.entry (.res .affine) <| letIn false (intLit 4) (mkres .affine (use 0))
    },
  { name := "cond_drop_affine",
    description := "An affine resource dropped explicitly in one arm of an if and left to scope exit on the other: accepted (the join sends it to MovedOut), one drop line either way. The bridge found the compiler ICEing on this (RUE-2290, fixed); the case stays as the regression signal.",
    rules := ["(@Drop) §5.3", "(If) §5.5 join", "3.9:38"],
    prog := Program.entry .int <| letIn false (mkres .affine (intLit 5))
      (seq (ite (boolLit true) (drop 0) unitLit) (intLit 9))
    },
  { name := "bool_result",
    description := "A boolean value from a comparison.",
    rules := ["§5.8 operator statics", "§6.4"],
    prog := Program.entry .bool <| lt (intLit 3) (intLit 2)
    },
  { name := "call_plain",
    description := "A plain call by value: main calls a two-parameter function that adds its parameters.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9"],
    prog := Examples.callPlain },
  { name := "return_past_affine",
    description := "An early return past two live affine bindings: the frame unwinds newest-first, so the drops print 4 then 3, then the value 7.",
    rules := ["(Return-Value) §5.7", "(D-Return) §6.9", "3.9:18", "3.9:4"],
    prog := Examples.returnPastAffine },
  { name := "return_past_linear",
    description := "An early return past a live linear binding: rejected statically (E0406, the §5.6 obligation at the ⊥_exit edge) and refused dynamically (linearLeak).",
    rules := ["(Return-Value) §5.7", "3.8:62", "§5.6 residual-linear leak check"],
    prog := Examples.returnPastLinear },
  { name := "param_dropped_at_frame_pop",
    description := "A by-value affine argument the callee never consumes: its drop runs at the frame pop, before the caller sees the value.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9", "3.8:62"],
    prog := Examples.paramDroppedAtPop },
  { name := "linear_param_leaked",
    description := "A by-value linear parameter the callee never consumes: (Fn) §5.8's second clause rejects the function (E0406) and the frame pop refuses with linearLeak.",
    rules := ["(Fn) §5.8", "3.8:62"],
    prog := Examples.linearParamLeaked },
  { name := "recursion_trap",
    description := "Recursion four frames deep, ending in a division by zero: a defined trap reached through a call chain.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "§6.4 arithmetic traps"],
    prog := Examples.recursionTrap },
  { name := "countdown",
    description := "A recursive countdown summing 4+3+2+1+0: every frame pops normally and the value comes back through five call boundaries.",
    rules := ["(Call) §5.8", "(D-Call) §6.9", "(D-Return-Value) §6.9"],
    prog := Examples.countdown }
]

/-! ## Witnesses for the refusals no `Examples.lean` program reaches -/

example : run (Program.entry .int (letIn true (mkres .linear (intLit 1))
    (seq (assign 0 (mkres .linear (intLit 2))) (consume (use 0))))) exportFuel
    = .stuck .linearOverwrite := by rfl
example : run (Program.entry .int (seq (mkres .linear (intLit 3)) (intLit 4))) exportFuel
    = .stuck .linearDiscard := by rfl
example : checkProgram (Program.entry .int (seq (mkres .linear (intLit 3)) (intLit 4)))
    = false := by rfl

/-! ## Outcomes, from the mechanization -/

/-- The printed line for a value the machine observes: a payload or a
scalar (`Print.observeValue`). -/
def valueLine : Val → Option String
  | .int n => some (toString n)
  | .bool b => some (if b then "true" else "false")
  | .unit => none
  | .res _ n => some (toString n)

/-- One stdout line per drop event: the dropped resource's payload. `eval`
emits events only for resource values (`dropRetire` for an affine one, the
`drop` and `seq` arms for any non-copy one), so the last case is unreachable.
It is written as a line no binary can print rather than as a `panic!`, so a
broken invariant fails the one case that has it — loudly, in the bridge's own
comparison — instead of aborting the whole export. -/
def eventLine : Event → String
  | .drop _ (.res _ n) => toString n
  | .dropTemp (.res _ n) => toString n
  | ev => s!"<drop event of a non-resource value: {repr ev}>"

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

/-- The stdout the bridge compares, for a completed run: one line per drop
event in trace order, then the program's value. -/
def outLines (v : Val) (tr : List Event) : List String :=
  tr.map eventLine ++ (valueLine v).toList

/-- A one-line reading of the outcome, for the program's header comment. -/
def outcomeSummary (c : Case) : String :=
  match checkProgram c.prog, run c.prog exportFuel with
  | false, .stuck w => "rejected by the checker; the machine would refuse with " ++ violationName w
  | false, .ok _ v tr =>
      let lines := outLines v tr
      "rejected by the checker; the refusal lies on a path not taken, and the executed path prints " ++
        (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | false, .panic k => "rejected by the checker; the refusal lies on a path not taken, and the executed path traps with " ++ panicName k
  | true, .ok _ v tr =>
      let lines := outLines v tr
      "accepted; prints " ++ (if lines.isEmpty then "nothing" else String.intercalate ", " lines) ++ "; exit 0"
  | true, .panic k => "accepted; traps with " ++ panicName k
  | true, .stuck w => "accepted yet refused with " ++ violationName w ++ " (impossible by soundness)"
  | _, .returned _ _ _ => "the entry call handed on a return (impossible: the call boundary absorbs it)"
  | _, .outOfFuel => "not completed at the export fuel; this case is not exported"

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

/-- The entry function's declared return type, which is the type the checker
gives the whole program (helper). -/
def resultTyName (c : Case) : String :=
  match c.prog[0]? with
  | some fd => Print.tyName fd.ret
  | none => Print.tyName .int

def verdictJson (c : Case) : String :=
  if checkProgram c.prog then
    "{\"accept\": {\"type\": " ++ jsonString (resultTyName c) ++ "}}"
  else "{\"reject\": {}}"

/-- The `expected` field. Two of its arms are unreachable in the exported
document and are here because the function must be total: a `returned` result,
which the entry call's own frame boundary absorbs (`run_ne_returned`), and an
`outOfFuel` one, which `jsonOf` filters out — the consumer knows only `ok`,
`panic` and `stuck` (`crates/rue-oracle-diff/src/lean_corpus.rs`). -/
def expectedJson (c : Case) : String :=
  match run c.prog exportFuel with
  | .ok _ v tr =>
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray ((outLines v tr).map jsonString) ++
        ", \"exit\": 0}"
  | .returned _ v tr =>
      "{\"kind\": \"ok\", \"stdout\": " ++ jsonArray ((outLines v tr).map jsonString) ++
        ", \"exit\": 0}"
  | .panic k => "{\"kind\": \"panic\", \"panic\": " ++ jsonString (panicName k) ++ "}"
  | .stuck w => "{\"kind\": \"stuck\", \"violation\": " ++ jsonString (violationName w) ++ "}"
  | .outOfFuel => "{\"kind\": \"outOfFuel\"}"

/-- Whether a case completed at the export fuel. A case that did not is left
out of the JSON: the interpreter has no outcome to claim for it (module
docstring). -/
def completed (c : Case) : Bool :=
  match run c.prog exportFuel with
  | .outOfFuel => false
  | _ => true

def caseJson (c : Case) : String :=
  "  {\n" ++
  "    \"name\": " ++ jsonString c.name ++ ",\n" ++
  "    \"description\": " ++ jsonString c.description ++ ",\n" ++
  "    \"rules\": " ++ jsonArray (c.rules.map jsonString) ++ ",\n" ++
  "    \"source\": " ++ jsonString (Print.program c.name c.description c.rules (outcomeSummary c) c.prog) ++ ",\n" ++
  "    \"verdict\": " ++ verdictJson c ++ ",\n" ++
  "    \"expected\": " ++ expectedJson c ++ "\n" ++
  "  }"

/-- A list of cases as one JSON document, the cases the export fuel did not
complete left out. -/
def jsonOf (cs : List Case) : String :=
  "[\n" ++ String.intercalate ",\n" ((cs.filter completed).map caseJson) ++ "\n]\n"

/-- The seed corpus as one JSON document. -/
def json : String := jsonOf cases

end RueCore.Corpus
