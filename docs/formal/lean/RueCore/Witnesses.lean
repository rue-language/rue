import RueCore.Adequacy
import RueCore.TraceExact
import RueCore.TraceOrder
import RueCore.Corpus

/-!
# RueCore.Witnesses — the theorems at work on the corpus programs (layer L3)

Kernel-checked facts about particular programs of `Examples.lean` and
`Corpus.lean`: one corpus program traced both ways (`Adequacy.lean`'s
theorems), the traces `drop_order` rejects and the order-witnessing corpus
cases read through it (`TraceOrder.lean`'s), and the check that the RUE-2316
carve-out excludes no seed case (`TraceExact.lean`'s).

They are moved here verbatim from those three modules (RUE-2456): they
mention example and corpus programs, which live in the tooling layer, and a
proof module must not import that layer (README, "Layers"). Nothing in the
proof modules depends on them.
-/

namespace RueCore

/-! ## One program, traced both ways -/

/-- The corpus case `affine_scope_drop` (`Corpus.lean`): `{ let v0: S1 =
S1 { x0: 7 }; 1 }` as the entry point returning `i64`, where `S1` is affine
with a destructor (helper). -/
abbrev affineScopeDropProgram : Program := Examples.prog Examples.tI64 Examples.affineDrop

/-- **One corpus program, both presentations** (GUIDE section 2, "One
program, traced both ways"; §6.2, §6.5, §6.7, §6.9, §6.11, §6.12). `check`
accepts `affine_scope_drop`; `run` answers `1` with both cells retired and
the trace "drop `ℓ1`, then `S1`'s destructor"; and §6's `→*` reaches the same
terminal configuration by the twelve steps written out here, one `Step`
constructor each: (Search) into the call's empty argument list, (D-Call),
(Search) into the `let`, (Search) into the struct literal and its one
initializer, the literal, the plug, (D-Struct) minting `#0`, (D-Let),
the body's literal, (D-EndScope) dropping and retiring `ℓ1`, and
(D-Return-Value). `explain/affine_scope_drop.txt` renders `eval`'s run of the
same program in seven rows: the (Search) steps are the part of `Step` that
`eval` does by recursion. -/
theorem affineScopeDrop_both_ways (M : FloatOps) :
    checkProgram affineScopeDropProgram = true ∧
    run M affineScopeDropProgram 100 =
      .ok [.dead, .dead] (.int .w64 .signed 1)
        [.drop 1 (.struct 1 0 [.int .w64 .signed 7]), .dtor 1 (.struct 1 0 [.int .w64 .signed 7])] ∧
    Steps M affineScopeDropProgram Config.init
      (.run [.dead, .dead] Frame.empty [] (.ret (.int .w64 .signed 1))
        [.drop 1 (.struct 1 0 [.int .w64 .signed 7]), .dtor 1 (.struct 1 0 [.int .w64 .signed 7])]) := by
  refine ⟨rfl, rfl, ?_⟩
  refine .step .callEnter ?_
  refine .step (.call rfl rfl rfl) ?_
  refine .step .letEnter ?_
  refine .step .structEnter ?_
  refine .step .argsPush ?_
  refine .step .intLit ?_
  refine .step .argsPlug ?_
  refine .step (.mkStruct rfl rfl) ?_
  refine .step .letBind ?_
  refine .step .intLit ?_
  refine .step (.endScope rfl) ?_
  refine .step (.callReturn rfl) ?_
  exact .refl _

open Examples in
/-- **Fields in declaration order, or the grammar rejects the trace**
(`3.9:13`): `struct_field_drop_order` drops `S7 { S1 {1}#0, S1 {2}#1 }#2`,
whose walk runs `#0`'s destructor and then `#1`'s. The same marker followed by
the two destructors the other way round is not in the grammar. -/
theorem fieldsSwapped_rejected :
    ¬ Blocks (prog tI64 structFieldOrder).decls
      [.drop 3 (.struct sTwoAffine 2 [cA 0 1, cA 1 2]), .dtor sAffine (cA 1 2),
        .dtor sAffine (cA 0 1)] := by
  intro h
  obtain ⟨t', ht, _⟩ := Blocks.drop_inv h
  have : dropEvents (prog tI64 structFieldOrder).decls (.struct sTwoAffine 2 [cA 0 1, cA 1 2])
      = [.dtor sAffine (cA 0 1), .dtor sAffine (cA 1 2)] := by rfl
  rw [this] at ht
  simp [cA, sAffine] at ht

open Examples in
/-- **Newest-first on a reachable step** (§6.9's (D-Return)):
`return_past_affine` returns past two live affine bindings, `ℓ1` then `ℓ3`,
and the one step that runs the frame's σ-walk drops `ℓ3` then `ℓ1` — two
distinct cells, newest first, the teardown `reachable_drop_order` speaks
of. -/
theorem returnPastAffine_newestFirst :
    ∃ C C' evs, Steps demoOps returnPastAffine Config.init C ∧
      Step demoOps returnPastAffine C C' ∧ C'.trace = C.trace ++ evs ∧ dropLocs evs = [3, 1] :=
  ⟨stepN demoOps returnPastAffine 18 Config.init, _, _, stepN_steps,
    step_iff.mpr rfl, rfl, rfl⟩

open Examples in
/-- **The invariant is what orders a teardown.** A frame whose scope record
is *not* in location order — `[3, 1]`, a configuration `reachable_ordered`
says no run reaches — pops (D-Return-Value) and drops `ℓ1` before `ℓ3`: the
step is a real step of §6's relation, and its markers are not newest-first.
So `step_drop_order`'s hypothesis is load-bearing, and `reachable_ordered`
is what discharges it. -/
theorem unorderedRecord_rejected :
    ∃ C C' evs, ¬ C.Ordered ∧ Step demoOps returnPastAffine C C' ∧
      C'.trace = C.trace ++ evs ∧ ¬ NewestFirst (dropLocs evs) := by
  refine ⟨.run [.dead, .full (cA 0 3), .dead, .full (cA 2 4)] { env := [3, 1], scope := [3, 1] }
      [.call { env := [], scope := [] }] (.ret (v64 7)) [], _, _,
    fun h => ?_, .callReturn rfl, rfl, fun h => ?_⟩
  · have := h.1.1; simp at this
  · change NewestFirst [1, 3] at h
    rcases h with ⟨ℓ, hℓ⟩ | h
    · have h1 := hℓ 1 (by decide); have h3 := hℓ 3 (by decide); omega
    · simp at h

open Examples in
/-- Two nested `let`s' pending markers, swapped: `endscope [1]` above
`endscope [3]` in a frame whose record is `[1, 3]` (the review's Probe 2)
(helper). -/
def swappedMarkers : Config :=
  .run [.dead, .full (cA 0 3), .dead, .full (cA 2 4)] { env := [3, 1], scope := [1, 3] }
    [.endscope [1], .endscope [3], .call { env := [], scope := [] }] (.ret (v64 7)) []

open Examples in
/-- **The nesting is what orders sibling scopes** (§6.7). With the two
markers swapped, every record is still in location order (`Config.Ordered`)
and every step drops one cell, so per-step order says nothing; yet the run
drops `ℓ1` before `ℓ3`, oldest first, and the first (D-EndScope) pops `ℓ3` off
the record while its marker drops `ℓ1`. `Config.Nested` rejects the
configuration, so `reachable_nested` says no run reaches it. -/
theorem swappedMarkers_rejected :
    swappedMarkers.Ordered ∧ ¬ swappedMarkers.Nested ∧
      ∃ C, Steps demoOps returnPastAffine swappedMarkers C ∧ dropLocs C.trace = [1, 3] := by
  refine ⟨⟨⟨by decide, by decide⟩, fun k hk => ?_⟩, fun h => ?_,
    ⟨stepN demoOps returnPastAffine 3 swappedMarkers, stepN_steps, rfl⟩⟩
  · simp only [List.mem_cons, List.not_mem_nil, or_false] at hk
    rcases hk with rfl | rfl | rfl <;> simp [Kont.Ordered, Rec] <;> decide
  · obtain ⟨⟨sc', hsc, _⟩, _⟩ := h
    have := congrArg List.reverse hsc
    simp at this

/-! ## The corpus, read through the theorems

The order-witnessing corpus cases, each run at the export model: the
identities the destructors ran on, in order (`dtorIds`, the order the printed
destructors print their payloads in, which the bridge already checks against
the compiler); the cells the `drop` markers name, in order (`dropLocs`); and
the owned identities the markers end (`freedIds`), each exactly once. -/

/-- A trace, read through the theorems: destructor identities, drop marker
cells, ended identities (helper). -/
def orderView (D : Decls) (tr : List Event) : List Nat × List Nat × List Nat :=
  (dtorIds tr, dropLocs tr, freedIds D tr)

/-- A corpus program's trace at the export model (helper). -/
def corpusTrace (P : Program) : List Event :=
  (run Corpus.exportOps P Examples.demoFuel).trace

open Examples in
/-- `struct_nested_dtor_drop`: the outer destructor (`#1`) before its field's
(`#0`), `3.9:28`. -/
example : orderView (prog tI64 structNestedDrop).decls (corpusTrace (prog tI64 structNestedDrop))
    = ([1, 0], [2], [1, 0]) := by rfl

open Examples in
/-- `struct_field_drop_order`: fields in declaration order, `3.9:13`. -/
example : orderView (prog tI64 structFieldOrder).decls (corpusTrace (prog tI64 structFieldOrder))
    = ([0, 1], [3], [2, 0, 1]) := by rfl

open Examples in
/-- `array_drop_order`: elements ascending, `3.9:15`. -/
example : orderView (prog tI64 arrayAffineDropOrder).decls
      (corpusTrace (prog tI64 arrayAffineDropOrder)) = ([0, 1, 2], [4], [3, 0, 1, 2]) := by rfl

open Examples in
/-- `array_elem_move_rest_ascending`: the moved element (`#1`) is dropped by
its new binding first, then the array's rest ascending, `3.8:73`. -/
example : orderView (prog tI64 arrayElemMove).decls (corpusTrace (prog tI64 arrayElemMove))
    = ([1, 0, 2], [5, 4], [1, 3, 0, 2]) := by rfl

open Examples in
/-- `enum_match_affine`: the shell (`#1`) consumed, the payload (`#0`)
dropped once by the arm's binding, `6.3:20`. -/
example : orderView (enumProg tI64 enumMatchAffine).decls
      (corpusTrace (enumProg tI64 enumMatchAffine)) = ([0], [3], [1, 0]) := by rfl

open Examples in
/-- `enum_two_payload_bindings`: the arm's two payload cells torn down in one
step, newest first (`ℓ5`, `ℓ4`). -/
example : orderView (enumProg tI64 enumTwoPayloadBindings).decls
      (corpusTrace (enumProg tI64 enumTwoPayloadBindings)) = ([1, 0], [5, 4], [2, 1, 0]) := by rfl

open Examples in
/-- `destructure_residue_order`: the residue in declaration order around the
leaf, both under `ℓ3`'s markers, §6.3. -/
example : orderView (destrProg tI64 destructureResidueOrder).decls
      (corpusTrace (destrProg tI64 destructureResidueOrder)) = ([0, 1], [3, 3], [0, 1, 2]) := by
  rfl

open Examples in
/-- `destructure_nested_residue`: nested residue before a later sibling. -/
example : orderView (destrProg tI64 destructureNestedResidue).decls
      (corpusTrace (destrProg tI64 destructureNestedResidue)) = ([0, 2], [4, 4], [0, 2, 3, 1]) := by
  rfl

open Examples in
/-- `nested_scopes`, the bridge case's own program (helper). -/
def nestedScopes : Program :=
  prog tI64 <| .letIn false (resA (lit 1)) (.letIn false (resA (lit 2)) (lit 0))

/-- It is the corpus case's program. -/
example : (Corpus.cases.find? (·.name == "nested_scopes")).map (·.prog.fns.map (·.body)) =
    some (nestedScopes.fns.map (·.body)) := by rfl

/-- `nested_scopes`: two sibling `let`s exit over two (D-EndScope) steps, the
inner (`ℓ3`) before the outer (`ℓ1`) — the cross-step order `reachable_lifo`
fixes, bridge-checked. -/
example : orderView nestedScopes.decls (corpusTrace nestedScopes) = ([2, 0], [3, 1], [2, 0]) := by
  rfl

open Examples in
/-- `return_past_affine`: the σ-walk newest first, `ℓ3` then `ℓ1`, §6.9. -/
example : orderView returnPastAffine.decls (corpusTrace returnPastAffine) = ([2, 0], [3, 1], [2, 0]) :=
  by rfl

open Examples in
/-- `two_params_dropped_at_pop`: the callee's frame pop tears its by-value
parameters down last-parameter first, `b` (`ℓ4`, the `S5` `#2` and its field
`#1`) before `a` (`ℓ3`, `#0`) — the LIFO half of `drop_order` at a frame,
bridge-checked. -/
example : orderView twoParamsDroppedAtPop.decls (corpusTrace twoParamsDroppedAtPop)
    = ([2, 1, 0], [4, 3], [2, 1, 0]) := by rfl

open Examples in
/-- `three_params_dropped_at_pop`: three parameters, `ℓ5`, `ℓ4`, `ℓ3`. -/
example : orderView threeParamsDroppedAtPop.decls (corpusTrace threeParamsDroppedAtPop)
    = ([2, 1, 0], [5, 4, 3], [2, 1, 0]) := by rfl

open Examples in
/-- `param_moved_other_dropped`: the first parameter, moved into `f2`, is
dropped by `f2`'s pop (`ℓ4`, `#0`); `f1`'s pop then owes only the second
(`ℓ3`, `#1`). -/
example : orderView paramMovedOtherDropped.decls (corpusTrace paramMovedOtherDropped)
    = ([0, 1], [4, 3], [0, 1]) := by rfl

open Examples in
/-- `loop_break_past_local`: the first turn's binding (`ℓ2`) is dropped by its
(D-EndScope) at the turn's end, and the second turn's (`ℓ4`) by the `break`'s
unwind, §6.10. -/
example : orderView (prog tI64 loopBreakPastLocal).decls
      (corpusTrace (prog tI64 loopBreakPastLocal)) = ([1, 3], [2, 4], [1, 3]) := by rfl

end RueCore

namespace RueCore.Corpus

-- Every seed case the checker accepts is `pendingSafe`: the RUE-2316
-- carve-out excludes nothing in the corpus, and a new seed that it would
-- exclude fails the build here.
#guard (cases.filter (fun c => checkProgram c.prog)).all (fun c => c.prog.pendingSafe)

end RueCore.Corpus

/-! ## Programs run through §6's relation

`Step.lean`'s demo witnesses, moved here verbatim (RUE-2460) so that the
definitions layer holds definitions only: programs run from §6.12's initial
configuration by `stepN` and read as `→*` derivations by `stepN_steps`
(`Step/Lemmas.lean`). They are checks on particular programs, not claims. -/

namespace RueCore

/-- A two-line program, `let x = 40; x + 2`, as the entry point returning
`i32` (helper). -/
def letAddProgram : Program :=
  Program.entry (Decls.ofStructs []) (.int .w32 .signed)
    (.letIn false (.intLit .w32 .signed 40) (.binop .add (.use (.var 0)) (.intLit .w32 .signed 2)))

/-- **The relation runs a program to the same answer `eval` does**, a check
the two presentations can be compared on before the adequacy theorems say
they always agree: from §6.12's initial configuration, `→*` reaches `✓42`
through (D-Call), (D-Let), (D-Use-Copy), (D-Arith), (D-EndScope) and
(D-Return-Value), with the `let`'s cell retired and nothing printed; and
`run` answers the same value, store and trace. -/
theorem letAddProgram_runs (M : FloatOps) :
    Steps M letAddProgram Config.init
      (.run [.dead] { env := [], scope := [] } [] (.ret (.int .w32 .signed 42)) []) ∧
    run M letAddProgram 100 = .ok [.dead] (.int .w32 .signed 42) [] :=
  ⟨stepN_steps (n := 30), rfl⟩


/-! ## Programs run through the relation

The review's repros (RUE-2324), each run from §6.12's initial configuration
by `stepN` and read as a `→*` derivation by `stepN_steps`. The first group pins
where §6 has no rule; the rest exercise drops, `loop`, `break`, `match` and
`return`, and `run` gives each of them the same answer. -/

/-- One affine struct with a destructor, `S`, and an affine enum
`E { A(S), B }` (helper). -/
def demoDecls : Decls :=
  { structs := [{ attr := .none, fields := [], dtor := true, cls := .affine }],
    enums := [{ variants := [[.struct 0], []], cls := .affine }] }

/-- A program over `demoDecls` whose entry point returns `i32` (helper). -/
def demoProgram (e : Expr) : Program := Program.entry demoDecls (.int .w32 .signed) e

/-- `S{}` and an `i32` literal (helper). -/
def demoS : Expr := .mkStruct 0 []

/-- An `i32` literal (helper). -/
def demoI32 (n : Int) : Expr := .intLit .w32 .signed n

/-- The contents `S{}` leaves in a cell, at the identity it was minted with
(helper). -/
def demoSc (i : Nat) : Contents := .struct 0 i []

/-- **(D-Use-Untrackable-Dynamic-Copy) needs `Copy`** (§6.3): in
`let a = [S{}, S{}]; let x = a[dyn 0]; 0` the dynamic read of an affine
leaf is stuck, before any destructor runs. `eval` is stuck at the same read
(`RueCore.Examples.dynReadAffine_refused`); `check` rejects the program. -/
theorem demo_dynamicRead_stuck (M : FloatOps) :
    ∃ C, Steps M (demoProgram (.letIn false (.mkArray (.struct 0) [demoS, demoS])
        (.letIn false (.indexRead (.var 0) [demoI32 0] [[]]) (demoI32 0)))) Config.init C ∧
      C.Stuck M (demoProgram (.letIn false (.mkArray (.struct 0) [demoS, demoS])
        (.letIn false (.indexRead (.var 0) [demoI32 0] [[]]) (demoI32 0)))) .typeConfusion :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- **`@drop` at a dynamic place needs `Copy`** (§6.3's only
`Untrackable(OrdinaryDynamic)` rule): `let a = [S{}]; @drop(a[dyn 0]); @dbg(1); 0`
is stuck at the `@drop`, with nothing printed. `eval` is stuck at the same
`@drop` (`RueCore.Examples.dynDropAffine_refused`). -/
theorem demo_dynamicDrop_stuck (M : FloatOps) :
    ∃ C, Steps M (demoProgram (.letIn false (.mkArray (.struct 0) [demoS])
        (.seq (.indexDrop (.var 0) [demoI32 0] [[]]) (.seq (.dbg (demoI32 1)) (demoI32 0)))))
        Config.init C ∧
      C.Stuck M (demoProgram (.letIn false (.mkArray (.struct 0) [demoS])
        (.seq (.indexDrop (.var 0) [demoI32 0] [[]]) (.seq (.dbg (demoI32 1)) (demoI32 0)))))
        .typeConfusion :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- **The repeat form needs `Copy`** (`7.1:38`): `let a = [S{}; 2]; 0` is
stuck at the repeat. `eval` is stuck at the same repeat
(`RueCore.Examples.repeatAffine_refused`). -/
theorem demo_repeat_stuck (M : FloatOps) :
    ∃ C, Steps M (demoProgram (.letIn false (.repeatArray (.struct 0) demoS 2) (demoI32 0)))
        Config.init C ∧
      C.Stuck M (demoProgram (.letIn false (.repeatArray (.struct 0) demoS 2) (demoI32 0)))
        .typeConfusion :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- **`@drop` of a moved-out place is a no-op** (§6.11: `drop(H, ⊘) = H`):
`let s = S{}; let t = s; @drop(s); 0` reaches `✓0`, and the one `S` is
destroyed once, when `t` goes out of scope. (`eval` refuses it with
`useAfterMove`; `check` rejects the program.) -/
theorem demo_dropMoved_runs (M : FloatOps) :
    ∃ H, Steps M (demoProgram (.letIn false demoS
        (.letIn false (.use (.var 0)) (.seq (.drop (.var 1)) (demoI32 0))))) Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w32 .signed 0))
        [.drop 2 (demoSc 0), .dtor 0 (demoSc 0)]) :=
  ⟨_, stepN_steps (n := 100)⟩

/-- **The loop yields `⟨⟩` to its context** (§6.10, RUE-2324's calculus
finding): `let x = loop { break }; @dbg(7); 0` prints `7` and reaches `✓0`. -/
theorem demo_loopInLet_runs (M : FloatOps) :
    ∃ H, Steps M (demoProgram (.letIn false (.loop .brk) (.seq (.dbg (demoI32 7)) (demoI32 0))))
      Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w32 .signed 0))
        [.dbg (.int .w32 .signed 7)]) ∧
    run M (demoProgram (.letIn false (.loop .brk) (.seq (.dbg (demoI32 7)) (demoI32 0)))) 100 =
      .ok H (.int .w32 .signed 0) [.dbg (.int .w32 .signed 7)] :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- **(D-Break) drops what the body owed** (§6.10's `unwind-drops`):
`loop { let s = S{}; break }; 3` destroys the `S` at the `break` and reaches
`✓3`. -/
theorem demo_breakDrops_runs (M : FloatOps) :
    ∃ H, Steps M (demoProgram (.seq (.loop (.letIn false demoS .brk)) (demoI32 3))) Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w32 .signed 3))
        [.drop 1 (demoSc 0), .dtor 0 (demoSc 0)]) ∧
    run M (demoProgram (.seq (.loop (.letIn false demoS .brk)) (demoI32 3))) 100 =
      .ok H (.int .w32 .signed 3) [.drop 1 (demoSc 0), .dtor 0 (demoSc 0)] :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- A counting loop: `let mut i = 0; loop { let s = S{}; if i >= 2 { break }
else { i = i + 1 } }; i` (helper). -/
def demoCountingLoop : Expr :=
  .letIn true (demoI32 0)
    (.seq (.loop (.letIn false demoS
        (.ite (.binop .ge (.use (.var 1)) (demoI32 2)) .brk
          (.assign (.var 1) (.binop .add (.use (.var 1)) (demoI32 1))))))
      (.use (.var 0)))

/-- **Every turn's drops run** (§6.7's (D-EndScope) on the turns that finish,
§6.10's (D-Break) on the one that breaks): the counting loop destroys three
`S`, one per turn, and reaches `✓2`. -/
theorem demo_loopTurns_runs (M : FloatOps) :
    ∃ H, Steps M (demoProgram demoCountingLoop) Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w32 .signed 2))
        [.drop 2 (demoSc 1), .dtor 0 (demoSc 1), .drop 4 (demoSc 3), .dtor 0 (demoSc 3),
         .drop 6 (demoSc 5), .dtor 0 (demoSc 5)]) ∧
    run M (demoProgram demoCountingLoop) 200 =
      .ok H (.int .w32 .signed 2)
        [.drop 2 (demoSc 1), .dtor 0 (demoSc 1), .drop 4 (demoSc 3), .dtor 0 (demoSc 3),
         .drop 6 (demoSc 5), .dtor 0 (demoSc 5)] :=
  ⟨_, stepN_steps (n := 300), rfl⟩

/-- **(D-Return) from inside a `let`** (§6.9): `let s = S{}; let y = return 5; 0`
discards the pending `let` and `endscope`, destroys the `S` from the frame's
record, and reaches `✓5`. -/
theorem demo_returnInLet_runs (M : FloatOps) :
    ∃ H, Steps M (demoProgram (.letIn false demoS (.letIn false (.ret (demoI32 5)) (demoI32 0))))
      Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w32 .signed 5))
        [.drop 1 (demoSc 0), .dtor 0 (demoSc 0)]) ∧
    run M (demoProgram (.letIn false demoS (.letIn false (.ret (demoI32 5)) (demoI32 0)))) 100 =
      .ok H (.int .w32 .signed 5) [.drop 1 (demoSc 0), .dtor 0 (demoSc 0)] :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- **(D-Return) from a `match` arm** (§6.6, §6.9):
`let x = S{}; match A(S{}) { A(p) => return 4, B => 0 }` destroys the arm's
payload and then `x`, newest first, and reaches `✓4`. The match consumes the
`A`'s shell first (`consume`, RUE-2427). -/
theorem demo_returnInMatch_runs (M : FloatOps) :
    ∃ H, Steps M (demoProgram (.letIn false demoS
        (.«match» (.mkEnum 0 0 [demoS]) [.ret (demoI32 4), demoI32 0]))) Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w32 .signed 4))
        [.consume (.enum 0 0 3 [.hole]), .drop 4 (demoSc 2), .dtor 0 (demoSc 2),
         .drop 1 (demoSc 0), .dtor 0 (demoSc 0)]) ∧
    run M (demoProgram (.letIn false demoS
        (.«match» (.mkEnum 0 0 [demoS]) [.ret (demoI32 4), demoI32 0]))) 100 =
      .ok H (.int .w32 .signed 4)
        [.consume (.enum 0 0 3 [.hole]), .drop 4 (demoSc 2), .dtor 0 (demoSc 2),
         .drop 1 (demoSc 0), .dtor 0 (demoSc 0)] :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- **(D-Loop-Iter) runs the turn's drops** (§6.10's `run-scope-drops`): at a
loop boundary whose frame owes nothing, a body value returned in a frame that
still owes cell 0 destroys it before the next turn. The configuration is not
reachable from `Config.init` — there `endscope` has always emptied the list —
but it is one §6.10's rule covers. -/
theorem demo_loopIter_drops (M : FloatOps) (e : Expr) :
    Step M (demoProgram e)
      (.run [.full (demoSc 0)] { env := [0], scope := [0] }
        [.loop .brk { env := [], scope := [] }] (.ret .unit) [])
      (.run [.dead] { env := [], scope := [] } [.loop .brk { env := [], scope := [] }]
        (.eval .brk) [.drop 0 (demoSc 0), .dtor 0 (demoSc 0)]) :=
  step_iff.mpr rfl

/-! ## `run_sim` and `run_complete`'s domain at work

Moved here verbatim from `Adequacy.lean` (RUE-2460): they are about the demo
programs above, which left the definitions layer with them. -/

/-- **Why completeness is stated on checked programs** (RUE-2314): in
`let s = S{}; let t = s; @drop(s); 0`, §6's `→*` reaches `✓0`, because §6.11
makes `@drop` of a `⊘` place a no-op (`demo_dropMoved_runs`, above).
`run` refuses it with `useAfterMove` instead. That refusal is the one disjunct
`run_complete` allows, and `check` rejects the program. -/
theorem dropMoved_refused (M : FloatOps) :
    (∃ H, Steps M (demoProgram (.letIn false demoS
        (.letIn false (.use (.var 0)) (.seq (.drop (.var 1)) (demoI32 0))))) Config.init
      (.run H Frame.empty [] (.ret (.int .w32 .signed 0))
        [.drop 2 (demoSc 0), .dtor 0 (demoSc 0)])) ∧
    run M (demoProgram (.letIn false demoS
        (.letIn false (.use (.var 0)) (.seq (.drop (.var 1)) (demoI32 0))))) 100 =
      .stuck .useAfterMove :=
  ⟨demo_dropMoved_runs M, rfl⟩

/-- **The theorem at work**: `letAddProgram_runs` (above) found its
`→*` derivation by running `stepN`; here it comes from `run`'s answer alone,
through `run_sim` — `let x = 40; x + 2` reaches `✓42` with the `let`'s cell
retired and nothing printed (§6.7, §6.9, §6.12). -/
theorem letAddProgram_sound (M : FloatOps) :
    Steps M letAddProgram Config.init
      (.run [.dead] Frame.empty [] (.ret (.int .w32 .signed 42)) []) :=
  (run_sim M letAddProgram 100).1 _ _ _ rfl

/-! ## The checker rejects each error class (RUE-2469)

The spine's program statements take `ProgramTyped`, which `checkProgram`
decides soundly (`checkProgram_sound`); `Spec.Nonvacuous` shows the hypothesis
holds of non-trivial programs, so the checker is not too strict to matter.
This section is the other side, for the checker's own profile: it **rejects**
a program of each error class the calculus's statics rule out, each a corpus
case named here, most beside an accepted corpus case that differs where the
error is. `lake exe ruecore-corpus --profile` counts acceptances and
rejections over the whole corpus and the generated programs. -/

/-- A corpus case's program, by its name (helper). -/
def caseProg? (name : String) : Option Program :=
  (Corpus.cases.find? (·.name == name)).map (·.prog)

/-- The error classes and their corpus witnesses: the class, a case the
checker must reject, and an accepted neighbour where the corpus has one
(helper). -/
def errorClassCases : List (String × String × Option String) := [
  ("use after move (a moved binding dropped again)", "use_after_move", some "affine_explicit_drop"),
  ("linear leak at a scope exit", "linear_leaked", some "linear_consumed"),
  ("linear leak at a branch join", "linear_half_consumed", none),
  ("linear leak on an early return", "return_past_linear", some "return_past_affine"),
  ("linear leak by a match arm", "enum_arm_leaks_payload", some "enum_arm_drops_payload"),
  ("linear element consumed on one path only", "array_linear_elem_one_path", none),
  ("linear value discarded by a sequence", "linear_temporary_discarded", none),
  ("linear value overwritten", "linear_overwrite", some "reinit"),
  ("use of a partially moved value", "partial_then_whole", some "drop_field_then_whole"),
  ("move out of a destructor-bearing value", "partial_under_dtor", some "partial_move_residue"),
  ("declared-linear destructure with a linear residue", "destructure_linear_residue",
    some "destructure_residue_order")]

/-- **`checkProgram` rejects a program of each error class** (§5's
premises, `3.8`): every rejected case of `errorClassCases` is in the corpus and
rejected, and every accepted neighbour is in the corpus and accepted, checked
by the kernel. -/
theorem errorClasses_rejected :
    errorClassCases.all (fun (_, bad, good) =>
      (caseProg? bad).map checkProgram == some false &&
        good.all fun g => (caseProg? g).map checkProgram == some true) = true := by
  decide

/-- **An operand of the wrong type, and a call of the wrong arity, are
rejected** (§5.8's (Arith) and (Call)): `1 + true`, and the entry point
calling itself with an argument it does not take. The corpus has neither,
since the compiler rejects both before the core. -/
theorem typeErrors_rejected :
    checkProgram (Program.entry (Decls.ofStructs []) (.int .w64 .signed)
        (.binop .add (.intLit .w64 .signed 1) (.boolLit true))) = false ∧
    checkProgram (Program.entry (Decls.ofStructs []) (.int .w64 .signed)
        (.call 0 [.intLit .w64 .signed 1])) = false := ⟨rfl, rfl⟩

end RueCore
