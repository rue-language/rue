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
