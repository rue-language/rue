//! The move/borrow/exclusivity state machine for one body analysis.
//!
//! [`OwnershipState`] owns every piece of per-body ownership bookkeeping the
//! engine threads through analysis: variable move states (including partial,
//! field-level moves), the scope frames that save shadowed move state, the
//! per-loop break/continue snapshots, active call loans, iteration borrows,
//! and the full-expression exclusivity ledgers. The state transitions live
//! here as methods so they can be unit-tested and evolved without the
//! engine's full surface (RUE-1802); `analysis/ownership.rs` remains the one
//! consumer that reads and writes the collections directly, and control-flow
//! joins snapshot and merge the whole lattice.

use ahash::{AHashMap, AHashSet};

use super::context::DivergenceKinds;
use lasso::Spur;
use rue_span::Span;

/// A path of field accesses from a root variable.
/// For example, `s.a.b` is represented as [sym("a"), sym("b")] with root sym("s").
///
/// Array element paths (RUE-186) reuse the same representation: a constant
/// index K is encoded as the interned decimal string of K (`xs[0]` →
/// [sym("0")]). Identifiers can never be all digits, so index segments can't
/// collide with field names.
pub(crate) type FieldPath = Vec<Spur>;

/// Tracks move state for a variable, including partial (field-level) moves.
// PartialEq is used by the loop back-edge move recheck to detect whether a
// loop body changed any move state. It is implemented by hand (below) rather
// than derived because `partial_moves` is an order-insensitive map stored as a
// `Vec`: two states with the same (path, span) entries in a different order
// must still compare equal, matching the `HashMap` this field replaced.
#[derive(Debug, Clone, Default)]
pub(crate) struct VariableMoveState {
    /// If Some, the entire variable has been fully moved at this span.
    ///
    /// This is MAY-move (union) information: after a branch join it is set if
    /// the variable was moved on ANY path, which is what the use-after-move
    /// check needs (a value that might be moved cannot be used).
    pub full_move: Option<Span>,
    /// True when the full move happened on EVERY non-diverging path reaching
    /// this point (intersection at branch joins). The linear must-consume
    /// check needs MUST-move information: a linear value consumed in only one
    /// branch of an `if`/`match` is still dropped on the other paths.
    /// Diverging branches (return/break/panic) never reach the join, so they
    /// don't participate in the intersection.
    pub full_move_on_all_paths: bool,
    /// Partial moves: maps field paths to the span where they were moved.
    /// For example, if `s.a` was moved, this contains ([sym("a")], span).
    ///
    /// Like `full_move`, this is MAY-move (union at branch joins).
    ///
    /// Stored as an association `Vec` rather than a `HashMap` (RUE-124): the
    /// typical variable has 0–5 partial moves, so a flat scan beats hashing,
    /// and it lets `is_path_moved` do a single O(n) prefix pass instead of an
    /// O(n×depth) per-ancestor probe. The invariant that each `FieldPath` key
    /// appears at most once (upheld by `mark_path_moved` and `merge_union`)
    /// makes it behave exactly like the map it replaced.
    pub partial_moves: Vec<(FieldPath, Span)>,
    /// Partial moves that happened on EVERY non-diverging path reaching this
    /// point (intersection at branch joins) — the per-path analogue of
    /// `full_move_on_all_paths`. Always a subset of `partial_moves`' keys.
    /// The element-wise linear array consumption check (RUE-186) needs
    /// MUST-move information: an element consumed in only one branch is
    /// still dropped on the other paths.
    pub partial_moves_on_all_paths: AHashSet<FieldPath>,
}

impl VariableMoveState {
    /// Mark a field path as moved.
    pub fn mark_path_moved(&mut self, path: &[Spur], span: Span) {
        if path.is_empty() {
            // Moving the whole variable
            self.full_move = Some(span);
            // A move on the straight-line path holds on every path until a
            // branch join intersects it away (see `merge_union`).
            self.full_move_on_all_paths = true;
            // Clear partial moves since the whole thing is moved
            self.partial_moves.clear();
            self.partial_moves_on_all_paths.clear();
        } else {
            // Partial move - only if not already fully moved
            if self.full_move.is_none() {
                // Upsert to preserve the unique-key invariant of the map this
                // `Vec` replaced: re-moving the same path overwrites its span
                // (matching `HashMap::insert`) rather than appending a dup.
                if let Some((_, existing)) = self.partial_moves.iter_mut().find(|(p, _)| p == path)
                {
                    *existing = span;
                } else {
                    self.partial_moves.push((path.to_vec(), span));
                }
                self.partial_moves_on_all_paths.insert(path.to_vec());
            }
        }
    }

    /// Mark a field path as reinitialized (assigned a fresh value): the exact
    /// path and any moved sub-paths under it are no longer moved.
    ///
    /// Does not affect `full_move`: writing one field back does not resurrect
    /// a fully moved variable (and assignment through a fully moved root is
    /// rejected before this is called).
    pub fn mark_path_reinitialized(&mut self, path: &[Spur]) {
        self.partial_moves
            .retain(|(moved, _)| !(moved.len() >= path.len() && moved[..path.len()] == *path));
        self.partial_moves_on_all_paths
            .retain(|moved| !(moved.len() >= path.len() && moved[..path.len()] == *path));
    }

    /// Check if a field path is moved.
    /// Returns Some(span) if the path (or any ancestor) is moved.
    pub fn is_path_moved(&self, path: &[Spur]) -> Option<Span> {
        // If fully moved, everything is moved
        if let Some(span) = self.full_move {
            return Some(span);
        }

        // Single O(n) scan over the partial moves (RUE-124), replacing the old
        // exact-lookup-plus-per-ancestor-probe (O(n×depth)). A stored path
        // `moved` affects `path` iff it is a prefix of it (an exact match or an
        // ancestor: `s.a` moved implies `s.a.b` is moved).
        //
        // When several stored paths match — possible only after a branch join
        // unions nested partials (e.g. both `s.a` and `s.a.b` moved on
        // different arms) — we must return the SAME span the old probe order
        // did: the exact match first, otherwise the shortest ancestor. `rank`
        // encodes that priority (exact = 0, then ancestor length ascending);
        // keys are unique, so no two candidates ever tie.
        let mut best: Option<(usize, Span)> = None;
        for (moved, span) in &self.partial_moves {
            let len = moved.len();
            if len <= path.len() && path[..len] == moved[..] {
                let rank = if len == path.len() { 0 } else { len };
                if best.is_none_or(|(best_rank, _)| rank < best_rank) {
                    best = Some((rank, *span));
                }
            }
        }
        best.map(|(_, span)| span)
    }

    /// Check whether `path` is valid to use as a WHOLE value (moving or
    /// passing the aggregate by value). Returns `Some(span)` if the path
    /// itself, any ancestor, OR any descendant has been moved.
    ///
    /// `is_path_moved` only checks the exact-and-ancestor direction, which is
    /// right for reading a single leaf: a moved *sibling* subfield is
    /// irrelevant to reading `o.inner.n`. But using `o.inner` as a whole value
    /// after `o.inner.s` was moved out would let the new owner and the hole
    /// coexist — spec 3.8 forbids using a struct with any moved field as a
    /// whole value, at any depth (RUE-279). So the whole-value-use sites must
    /// also reject when a DESCENDANT path (a stored moved path that has `path`
    /// as a strict prefix) is moved.
    pub fn is_path_or_descendant_moved(&self, path: &[Spur]) -> Option<Span> {
        if let Some(span) = self.is_path_moved(path) {
            return Some(span);
        }
        // Descendant direction: a longer stored moved path (`o.inner.s`) whose
        // prefix is the queried whole-value path (`o.inner`).
        self.partial_moves
            .iter()
            .find(|(moved, _)| moved.len() > path.len() && moved[..path.len()] == *path)
            .map(|(_, span)| *span)
    }

    /// Check if the entire variable (including all fields) is fully valid to use.
    /// Returns Some(span) if there's any move (full or partial) that would prevent use.
    pub fn is_any_part_moved(&self) -> Option<Span> {
        if let Some(span) = self.full_move {
            return Some(span);
        }
        self.partial_moves.first().map(|(_, span)| *span)
    }

    /// Check if the variable has any move state.
    pub fn is_empty(&self) -> bool {
        self.full_move.is_none() && self.partial_moves.is_empty()
    }

    /// Merge move states from two branches (union semantics).
    /// A variable is considered moved after a branch if it was moved in EITHER branch.
    /// This prevents use-after-move when a value might have been moved.
    ///
    /// The one intersection: `full_move_on_all_paths` survives only if BOTH
    /// branches fully moved the value (must-move, for the linear
    /// must-consume check).
    pub fn merge_union(branch1: &Self, branch2: &Self) -> Self {
        // If either branch has a full move, the result is a full move
        // (use the span from whichever branch has it, preferring branch1)
        let full_move = branch1.full_move.or(branch2.full_move);

        // A partial move is kept if it appears in EITHER branch. When both
        // branches moved the same path, branch1's span wins (`or_insert`
        // semantics of the map this replaced): start from branch1's list and
        // append only paths branch2 introduced.
        let mut partial_moves = branch1.partial_moves.clone();
        for (path, span) in &branch2.partial_moves {
            if !partial_moves.iter().any(|(p, _)| p == path) {
                partial_moves.push((path.clone(), *span));
            }
        }

        // A path was moved on EVERY path only if both branches moved it.
        // A branch that fully moved the value on all its paths covers every
        // path (it has no per-path set of its own — the full move subsumed
        // it), so the other branch's set survives.
        let partial_moves_on_all_paths = if branch1.full_move_on_all_paths {
            branch2.partial_moves_on_all_paths.clone()
        } else if branch2.full_move_on_all_paths {
            branch1.partial_moves_on_all_paths.clone()
        } else {
            branch1
                .partial_moves_on_all_paths
                .intersection(&branch2.partial_moves_on_all_paths)
                .cloned()
                .collect()
        };

        Self {
            full_move,
            full_move_on_all_paths: branch1.full_move_on_all_paths
                && branch2.full_move_on_all_paths,
            partial_moves,
            partial_moves_on_all_paths,
        }
    }
}

// Hand-written so `partial_moves` compares as an unordered (path -> span) map,
// matching the `HashMap` it replaced (RUE-124). The unique-key invariant lets
// equal length + one-directional containment stand in for full set equality.
// The loop back-edge recheck relies on this being order-insensitive.
impl PartialEq for VariableMoveState {
    fn eq(&self, other: &Self) -> bool {
        self.full_move == other.full_move
            && self.full_move_on_all_paths == other.full_move_on_all_paths
            && self.partial_moves_on_all_paths == other.partial_moves_on_all_paths
            && self.partial_moves.len() == other.partial_moves.len()
            && self.partial_moves.iter().all(|(p, s)| {
                other
                    .partial_moves
                    .iter()
                    .any(|(op, os)| op == p && os == s)
            })
    }
}

/// Apply one scope's saved move-state frame to a move map, restoring each
/// name declared in that scope to its pre-declaration state (or absence), in
/// reverse declaration order — the RUE-522 restoration `pop_scope` performs
/// on the live `moved_vars`, factored out so a loop can apply it to its
/// break-site snapshots too (RUE-1293): a snapshot taken inside the loop's
/// scope may carry loop-local names, and for a name that *shadows* an outer
/// binding the snapshot's entry describes the dead inner binding, while the
/// outer binding's state at the break is exactly the saved entry this
/// restoration writes back.
pub(crate) fn restore_scope_moves(
    moves: &mut AHashMap<Spur, VariableMoveState>,
    frame: &[(Spur, Option<VariableMoveState>)],
) {
    for (symbol, old_moves) in frame.iter().rev() {
        match old_moves {
            Some(state) => {
                moves.insert(*symbol, state.clone());
            }
            None => {
                moves.remove(symbol);
            }
        }
    }
}

/// The move states one enclosing loop's `break`s and `continue`s have
/// established so far (RUE-1293).
///
/// Both are diverging edges, so the `if`/`match` joins correctly exclude
/// their states from the fall-through — but each state still arrives
/// somewhere: a `break`'s at the code AFTER the loop (the exit is the union
/// of the break states; formal core §5.7, (Loop-Break)), and a `continue`'s
/// at the next iteration's entry (the back edge is the union of the
/// fall-through and continue states). Before this record collected them,
/// both edges dropped their move states entirely, accepting use-after-move
/// with an observable double-drop through either a post-loop use (break) or
/// a next-iteration use (continue).
#[derive(Debug, Clone, Default)]
pub(crate) struct LoopEdgeStates {
    /// A `break` targeting this loop exists: the loop is `()`-typed.
    pub broke: bool,
    /// One `moved_vars` snapshot per `break` targeting this loop.
    pub break_snaps: Vec<(AHashMap<Spur, VariableMoveState>, usize)>,
    /// One `moved_vars` snapshot per `continue` targeting this loop.
    pub continue_snaps: Vec<(AHashMap<Spur, VariableMoveState>, usize)>,
    /// Index of the first scope frame a `break`/`continue` targeting this
    /// loop unwinds — the loop's own scope frame, recorded when the loop
    /// pushes it. The early-exit edge walk (RUE-1614) enforces the linear
    /// must-consume obligation over exactly the frames `first_unwound_frame..`
    /// at each such edge: those scopes end at the edge and their live
    /// bindings are dropped there, while bindings outside the loop stay live
    /// and may still be consumed after it.
    pub first_unwound_frame: usize,
}

/// Record one snapshot in a per-loop edge list, merging with an existing
/// same-depth snapshot (union — moved on either edge ⇒ moved at the join).
///
/// Each snapshot is tagged with the scope depth (`moved_scope_stack.len()`)
/// at which it was taken: it names whatever bindings were visible at the
/// edge, so as each intervening scope pops, `pop_scope` replays that scope's
/// RUE-522 restoration onto every snapshot taken inside it (and lowers its
/// tag) — a shadowed outer binding resumes its own state, and scope locals
/// drop out. By the time the loop pops its record, every snapshot describes
/// the post-loop scope view. Eager same-depth merging keeps each list as
/// small as the loop's live scope nesting.
fn record_edge_snapshot(
    snaps: &mut Vec<(AHashMap<Spur, VariableMoveState>, usize)>,
    moves: &AHashMap<Spur, VariableMoveState>,
    depth: usize,
) {
    if let Some((existing, existing_depth)) = snaps.last_mut()
        && *existing_depth == depth
    {
        *existing = union_move_maps(existing, moves);
        return;
    }
    snaps.push((moves.clone(), depth));
}

/// Union-merge a drained edge list, or `None` when the edge never fired.
fn merged_edge_moves(
    snaps: Vec<(AHashMap<Spur, VariableMoveState>, usize)>,
) -> Option<AHashMap<Spur, VariableMoveState>> {
    let mut snaps = snaps.into_iter();
    let (mut merged, _) = snaps.next()?;
    for (snap, _) in snaps {
        merged = union_move_maps(&merged, &snap);
    }
    Some(merged)
}

impl LoopEdgeStates {
    /// A fresh record for a loop whose own (just-pushed) scope frame is
    /// `first_unwound_frame` (RUE-1614).
    pub fn entered_at(first_unwound_frame: usize) -> Self {
        LoopEdgeStates {
            first_unwound_frame,
            ..LoopEdgeStates::default()
        }
    }

    /// Record one break's snapshot.
    pub fn record_break(&mut self, moves: &AHashMap<Spur, VariableMoveState>, depth: usize) {
        self.broke = true;
        record_edge_snapshot(&mut self.break_snaps, moves, depth);
    }

    /// Record one continue's snapshot.
    pub fn record_continue(&mut self, moves: &AHashMap<Spur, VariableMoveState>, depth: usize) {
        record_edge_snapshot(&mut self.continue_snaps, moves, depth);
    }

    /// The union-merge of every break snapshot — the loop's exit ownership
    /// state — or `None` when the loop has no break; and of every continue
    /// snapshot — the back edge's addition to the fall-through state.
    pub fn merged_moves(
        self,
    ) -> (
        Option<AHashMap<Spur, VariableMoveState>>,
        Option<AHashMap<Spur, VariableMoveState>>,
    ) {
        (
            merged_edge_moves(self.break_snaps),
            merged_edge_moves(self.continue_snaps),
        )
    }
}

/// Union-merge two branch move-state maps (see [`VariableMoveState::merge_union`]).
///
/// A variable with state in only one map is merged against the default
/// (no-moves) state, which correctly clears `full_move_on_all_paths`: the
/// other branch did not move it.
pub(crate) fn union_move_maps(
    branch1: &AHashMap<Spur, VariableMoveState>,
    branch2: &AHashMap<Spur, VariableMoveState>,
) -> AHashMap<Spur, VariableMoveState> {
    let default = VariableMoveState::default();
    let mut merged = AHashMap::new();
    for symbol in branch1.keys().chain(branch2.keys()) {
        if merged.contains_key(symbol) {
            continue;
        }
        let state1 = branch1.get(symbol).unwrap_or(&default);
        let state2 = branch2.get(symbol).unwrap_or(&default);
        let merged_state = VariableMoveState::merge_union(state1, state2);
        if !merged_state.is_empty() {
            merged.insert(*symbol, merged_state);
        }
    }
    merged
}

/// How a call argument (or method receiver) loans its root variable for the
/// duration of the call — the two by-ref modes tracked in
/// [`OwnershipState::call_loaned_roots`]. Carried in the loan frame so the
/// E0208 diagnostic can name the conflicting keyword (RUE-523).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallLoanKind {
    Inout,
    Borrow,
}

impl CallLoanKind {
    /// The source keyword, for diagnostics.
    pub(crate) fn keyword(self) -> &'static str {
        match self {
            CallLoanKind::Inout => "inout",
            CallLoanKind::Borrow => "borrow",
        }
    }
}

pub(crate) struct FullExpressionBoundary {
    loans: usize,
    shared_reads: Vec<(Spur, Span)>,
    exclusive_uses: Vec<(Spur, Span)>,
}

#[derive(Clone, Copy)]
pub(crate) struct ExpressionLedgerCheckpoint {
    loans: usize,
    shared_reads: usize,
    exclusive_uses: usize,
}

/// The complete move/borrow/exclusivity state threaded through one body
/// analysis, embedded in `AnalysisContext` as its `ownership` field.
///
/// A fresh body starts from `OwnershipState::default()`: nothing moved, no
/// open scopes, no active loans.
#[derive(Clone, Default)]
pub(crate) struct OwnershipState {
    /// Variables that have been moved (for affine type checking).
    /// Maps variable symbol to move state (supports partial/field-level moves).
    pub moved_vars: AHashMap<Spur, VariableMoveState>,
    /// Per-scope saved MOVE states, parallel to the context's `scope_stack`:
    /// each frame holds one (symbol, shadowed binding's move state) entry per
    /// binding introduced in that scope. `moved_vars` is keyed by NAME, so a
    /// shadowing declaration must save the outer binding's state here and
    /// [`Self::pop_scope_frame`] must restore it — otherwise a moved outer
    /// binding is resurrected by a same-named inner `let`/match binding
    /// (double destruction) or a move of the inner shadow outlives its block
    /// and poisons the outer binding with a false E0205 (RUE-522).
    pub moved_scope_stack: Vec<Vec<(Spur, Option<VariableMoveState>)>>,
    /// One entry per enclosing loop (innermost last), recording what that
    /// loop's diverging edges establish: whether a `break` exists at all (an
    /// infinite loop containing one has type `()`; without one it has type
    /// `!` — spec 4.8), and the move-state snapshots at the break and
    /// continue sites. The break states are the loop's exit ownership state,
    /// and the continue states join the fall-through as the back-edge state
    /// (RUE-1293; formal core §5.7, (Loop-Break)) — see [`LoopEdgeStates`].
    pub loop_break_stack: Vec<LoopEdgeStates>,
    /// True while re-running a loop's condition/body to validate the loop's
    /// back edge (see [`Self::fork_for_recheck`]). The recheck pass starts
    /// from a move state that already includes every move the loop performs,
    /// so nested loops analyzed within it don't need (and must not trigger)
    /// their own recheck — that would make nested loops exponential.
    pub in_loop_move_recheck: bool,
    /// While analyzing the value of an `inout`/`borrow` call argument, the ROOT
    /// variable of the place being passed by reference (set by
    /// `analyze_call_args_coerced`; for `f(borrow o.f)` this is `o`). A by-ref argument
    /// is a borrow, not a move, so the variable-reference and place analyses
    /// must not mark the root (or the projected path) as moved — and must not
    /// reject forwarding a by-ref parameter to another function's by-ref
    /// parameter (RUE-143).
    pub byref_arg_root: Option<Spur>,
    /// The exact root place whose `@drop` operand is being analyzed. This is
    /// the one by-value consumer that may accept an already-moved descendant:
    /// the drop elaborator destroys the owned residue immediately. Keeping the
    /// root here prevents nested operand work from relaxing checks for another
    /// moved value.
    pub drop_intrinsic_operand: Option<Spur>,
    /// Stack of loan frames, one per call whose argument list is currently
    /// being analyzed (outermost first): the ROOT variables that call passes
    /// `inout`/`borrow`, plus a by-ref method receiver's root. A loan spans
    /// the entire call, so recording a MOVE of any root on this stack while
    /// evaluating another argument of the same call (directly, or nested —
    /// `f(inout x, g(x))`) would leave the loan aliasing moved-from storage:
    /// a double free in safe code. Move-record sites consult this via the
    /// engine's `reject_move_of_call_loaned_root` (E0208, spec 6.1, RUE-523).
    /// Completed nested LOANS (`f(inout x, g(borrow x))`) do not conflict:
    /// the inner loan ends before the outer call begins. The third field is
    /// true when the enclosing by-ref argument is materialized as a view
    /// value (for example `borrow str`, or `StrBuf` narrowed to `inout str`),
    /// rather than passed by address; nested exclusive access must not
    /// invalidate such a snapshot.
    pub call_loaned_roots: Vec<Vec<(Spur, CallLoanKind, bool)>>,
    /// Collection variables currently held as a scoped shared borrow by an
    /// enclosing `for` loop (innermost last), spec 4.8:26 / RUE-233. While a
    /// name is here, any mutation of it in the loop body — whole-variable
    /// assignment (`a = …`), field set (`a.f = …`), or element set
    /// (`a[i] = …`) — is rejected with E0428 (`MutateBorrowedValue`), matching
    /// the treatment of an explicit `borrow` parameter. Reads (including the
    /// loop's own element reads) are permitted.
    pub iter_borrows: Vec<Spur>,
    /// Active accessor-result loans for the current full expression
    /// (ADR-0062): each entry is the receiver root, call span, and shared or
    /// exclusive mode of an expanded accessor call. The statement loop
    /// truncates this to its pre-statement length after every statement, so
    /// an entry's extent is exactly the enclosing full expression. An
    /// incompatible use of a listed root within that extent is E0259.
    pub expression_loans: Vec<(Spur, Span, CallLoanKind)>,
    /// Ordinary shared reads in the current full expression. An exclusive
    /// accessor result conflicts with these reads in either evaluation order.
    pub expression_shared_reads: Vec<(Spur, Span)>,
    /// Completed exclusive uses in the current full expression. An accessor
    /// result conflicts with a prior use of the same root even when that use
    /// was a nested call whose own loan frame has already ended.
    pub expression_exclusive_uses: Vec<(Spur, Span)>,
}

impl OwnershipState {
    /// A (re)declaration of `symbol` is a fresh binding, so it starts with no
    /// moves — but the shadowed binding's move state is SAVED in the current
    /// scope's move frame and restored by [`Self::pop_scope_frame`], because
    /// `moved_vars` is keyed by name, not binding identity (RUE-522). Applies
    /// to every scoped binding form: nested `let`, `match` payload bindings,
    /// loop binders, and a comptime type alias hiding a runtime local.
    pub fn bind_fresh(&mut self, symbol: Spur) {
        let old_moves = self.moved_vars.remove(&symbol);
        if let Some(move_frame) = self.moved_scope_stack.last_mut() {
            move_frame.push((symbol, old_moves));
        }
    }

    /// Open one scope's move frame, parallel to the context's scope stack.
    pub fn push_scope_frame(&mut self) {
        self.moved_scope_stack.push(Vec::new());
    }

    /// Close the innermost scope's move frame: restore each shadowed
    /// binding's move state (RUE-522), and replay the same restoration onto
    /// every loop break/continue snapshot taken inside the popped scope
    /// (RUE-1293) — a snapshot names whatever bindings were visible at its
    /// edge, so a shadowed outer binding resumes its own state and scope
    /// locals drop out. Every record on the stack encloses the popping scope
    /// (an inner loop's record is pushed and popped strictly within one
    /// enclosing scope), so only the depth tag decides applicability.
    pub fn pop_scope_frame(&mut self) {
        let depth_before_pop = self.moved_scope_stack.len();
        if let Some(move_frame) = self.moved_scope_stack.pop() {
            restore_scope_moves(&mut self.moved_vars, &move_frame);
            for record in &mut self.loop_break_stack {
                for (snap, depth) in record
                    .break_snaps
                    .iter_mut()
                    .chain(record.continue_snaps.iter_mut())
                {
                    if *depth >= depth_before_pop {
                        restore_scope_moves(snap, &move_frame);
                        *depth = depth_before_pop - 1;
                    }
                }
            }
        }
    }

    /// The ownership half of the loop back-edge recheck fork: everything
    /// carries over — a value moved anywhere in a loop's condition or body is
    /// already moved when the back edge re-enters the loop — except the two
    /// per-operand markers, which can never be live between whole-expression
    /// analyses (a by-ref argument's value is a place, and index
    /// subexpressions are analyzed with the root cleared), and the recheck
    /// flag itself, which stops nested loops from forking their own recheck.
    /// The enclosing calls' loan frames stay visible because an argument
    /// value may contain a loop (`f(inout x, { while … })`).
    pub fn fork_for_recheck(&self) -> Self {
        Self {
            moved_vars: self.moved_vars.clone(),
            moved_scope_stack: self.moved_scope_stack.clone(),
            loop_break_stack: self.loop_break_stack.clone(),
            in_loop_move_recheck: true,
            byref_arg_root: None,
            drop_intrinsic_operand: None,
            call_loaned_roots: self.call_loaned_roots.clone(),
            iter_borrows: self.iter_borrows.clone(),
            expression_loans: self.expression_loans.clone(),
            expression_shared_reads: self.expression_shared_reads.clone(),
            expression_exclusive_uses: self.expression_exclusive_uses.clone(),
        }
    }

    /// Snapshot expression-scoped semantic records before a strict operation.
    /// If that operation does not continue, records created by its operands
    /// cannot affect later unreachable analysis.
    pub(crate) fn checkpoint_expression_ledgers(&self) -> ExpressionLedgerCheckpoint {
        ExpressionLedgerCheckpoint {
            loans: self.expression_loans.len(),
            shared_reads: self.expression_shared_reads.len(),
            exclusive_uses: self.expression_exclusive_uses.len(),
        }
    }

    pub(crate) fn rollback_expression_ledgers(&mut self, checkpoint: ExpressionLedgerCheckpoint) {
        self.expression_loans.truncate(checkpoint.loans);
        self.expression_shared_reads
            .truncate(checkpoint.shared_reads);
        self.expression_exclusive_uses
            .truncate(checkpoint.exclusive_uses);
    }

    /// Start a nested full expression. Active loans from the enclosing
    /// expression remain visible, while completed read/use records are
    /// isolated until the child finishes.
    pub(crate) fn enter_full_expression(&mut self) -> FullExpressionBoundary {
        FullExpressionBoundary {
            loans: self.expression_loans.len(),
            shared_reads: std::mem::take(&mut self.expression_shared_reads),
            exclusive_uses: std::mem::take(&mut self.expression_exclusive_uses),
        }
    }

    /// The accessor loans a nested full expression established and still
    /// holds — the loans of its own tail expression, since every nested
    /// statement inside it already discarded its own at its boundary.
    ///
    /// Read just before [`Self::exit_full_expression`] by an `if`/`match` arm
    /// whose value flows on as the join's value: that value is an accessor
    /// result of the ENCLOSING full expression, so its loan keeps the 6.6:10
    /// extent the arm boundary would otherwise cut short (RUE-1678). The
    /// caller re-registers the returned loans with
    /// [`Self::readmit_expression_loans`] after the boundary closes.
    pub(crate) fn nested_expression_loans(
        &self,
        boundary: &FullExpressionBoundary,
    ) -> Vec<(Spur, Span, CallLoanKind)> {
        self.expression_loans[boundary.loans..].to_vec()
    }

    /// Re-register loans harvested by [`Self::nested_expression_loans`] into
    /// the enclosing full expression (RUE-1678).
    pub(crate) fn readmit_expression_loans(&mut self, loans: Vec<(Spur, Span, CallLoanKind)>) {
        self.expression_loans.extend(loans);
    }

    /// Finish a nested full expression, discarding child loans and restoring
    /// the enclosing expression's completed read/use records.
    pub(crate) fn exit_full_expression(&mut self, boundary: FullExpressionBoundary) {
        self.expression_loans.truncate(boundary.loans);
        self.expression_shared_reads = boundary.shared_reads;
        self.expression_exclusive_uses = boundary.exclusive_uses;
    }

    /// Merge move states from two branches.
    ///
    /// For if-else expressions, a variable is considered moved after the expression
    /// if it was moved in EITHER branch (union semantics). This prevents use-after-move
    /// when a value might have been moved in one branch:
    ///
    /// ```rue
    /// if cond { consume(x) } else { }
    /// x  // Error: x might have been moved in the then-branch
    /// ```
    ///
    /// When one branch diverges (returns Never), only the other branch's moves matter:
    /// - If then-branch diverges, else-branch's moves are used (then never returns)
    /// - If else-branch diverges, then-branch's moves are used (else never returns)
    /// - If both diverge, explicit-panic edges are exempt; unchecked edge
    ///   state is retained for the enclosing conservative check
    pub fn merge_branch_moves(
        &mut self,
        then_moves: AHashMap<Spur, VariableMoveState>,
        else_moves: AHashMap<Spur, VariableMoveState>,
        then_diverges: bool,
        else_diverges: bool,
        then_divergence: DivergenceKinds,
        else_divergence: DivergenceKinds,
    ) {
        // If then-branch diverges, use else-branch's moves
        // If else-branch diverges, use then-branch's moves
        // If both diverge, the whole expression diverges - doesn't matter what we do
        // If neither diverges, merge the moves (union - moved in either = moved after)
        match (then_diverges, else_diverges) {
            (true, true) => {
                // Both branches diverge, but the enclosing block may still
                // inspect this state when the edges have different
                // provenance (for example panic plus a checked return).
                // A sole unchecked edge supplies the state checked by the
                // enclosing block; otherwise unioning is independent of arm
                // order, and exempt-only blocks skip the check altogether.
                self.moved_vars = if then_divergence.has_other() && !else_divergence.has_other() {
                    then_moves
                } else if else_divergence.has_other() && !then_divergence.has_other() {
                    else_moves
                } else if !then_divergence.has_other() && !else_divergence.has_other() {
                    then_moves
                } else {
                    union_move_maps(&then_moves, &else_moves)
                };
            }
            (true, false) => {
                // Then-branch diverges, else-branch continues.
                // Use else-branch's moves (then never executes to completion).
                self.moved_vars = else_moves;
            }
            (false, true) => {
                // Else-branch diverges, then-branch continues.
                // Use then-branch's moves (else never executes to completion).
                self.moved_vars = then_moves;
            }
            (false, false) => {
                // Neither diverges - merge the moves (union).
                // A variable is moved after if-else if moved in EITHER branch
                // (and fully-moved-on-all-paths only if moved in BOTH).
                self.moved_vars = union_move_maps(&then_moves, &else_moves);
            }
        }
    }

    /// Merge move states captured from the arms of a `match`.
    ///
    /// Exactly one arm executes, so this is [`Self::merge_branch_moves`]
    /// generalized to N branches: a value is moved after the match if it was
    /// moved in ANY non-diverging arm (union), and a linear value counts as
    /// consumed on all paths only if EVERY non-diverging arm consumed it
    /// (`full_move_on_all_paths` intersects in `merge_union`). Diverging arms
    /// never reach the join, so they are excluded; if every arm diverges, a
    /// explicit-panic arm is excluded from the conservative residual state;
    /// unchecked non-panic arms remain available for conservative validation,
    /// while checked exits do not contaminate it.
    ///
    /// Each entry is the `moved_vars` snapshot after analyzing one arm
    /// (starting from the same pre-match state), paired with whether that
    /// arm's body diverges and its reachable provenance.
    pub fn merge_arm_moves(
        &mut self,
        arm_moves: Vec<(AHashMap<Spur, VariableMoveState>, bool, DivergenceKinds)>,
    ) {
        let mut live = arm_moves
            .iter()
            .filter(|(_, diverges, _)| !diverges)
            .map(|(moves, _, _)| moves);

        let Some(first) = live.next() else {
            // Every arm diverges. (A zero-arm match on a zero-variant enum
            // returns early in analyze_match and never reaches this merge.)
            let nonpanic: Vec<_> = arm_moves
                .iter()
                .filter(|(_, _, kinds)| kinds.has_other())
                .map(|(moves, _, _)| moves)
                .collect();
            let source: Vec<_> = if !nonpanic.is_empty() {
                nonpanic
            } else {
                arm_moves.iter().map(|(moves, _, _)| moves).collect()
            };
            let Some(first) = source.first() else {
                self.moved_vars = AHashMap::new();
                return;
            };
            let mut merged = (*first).clone();
            for moves in source.into_iter().skip(1) {
                merged = union_move_maps(&merged, moves);
            }
            self.moved_vars = merged;
            return;
        };

        let mut merged = first.clone();
        for arm in live {
            merged = union_move_maps(&merged, arm);
        }
        self.moved_vars = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use rue_span::Span;

    fn syms(names: &[&str]) -> Vec<Spur> {
        let interner = ThreadedRodeo::default();
        names.iter().map(|n| interner.get_or_intern(n)).collect()
    }

    #[test]
    fn variable_move_state_default_is_empty() {
        let state = VariableMoveState::default();
        assert!(state.full_move.is_none());
        assert!(state.partial_moves.is_empty());
        assert!(state.is_empty());
    }

    #[test]
    fn variable_move_state_full_move() {
        let mut state = VariableMoveState::default();
        let span = Span::new(10, 20);
        state.mark_path_moved(&[], span);

        assert!(state.full_move.is_some());
        assert_eq!(state.full_move.unwrap(), span);
        assert!(state.partial_moves.is_empty()); // Full move clears partials
    }

    #[test]
    fn variable_move_state_is_path_moved_after_full_move() {
        let mut state = VariableMoveState::default();
        let span = Span::new(10, 20);
        state.mark_path_moved(&[], span);

        // Any path should be considered moved after a full move
        assert_eq!(state.is_path_moved(&[]), Some(span));

        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        assert_eq!(state.is_path_moved(&[field_x]), Some(span));
    }

    #[test]
    fn variable_move_state_partial_move() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span = Span::new(10, 20);

        state.mark_path_moved(&[field_x], span);

        assert!(state.full_move.is_none());
        assert_eq!(state.partial_moves.len(), 1);
        assert_eq!(state.is_path_moved(&[field_x]), Some(span));
    }

    #[test]
    fn variable_move_state_partial_move_does_not_affect_root() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span = Span::new(10, 20);

        state.mark_path_moved(&[field_x], span);

        // The root path should not be moved if only a field is moved
        assert!(state.is_path_moved(&[]).is_none());
    }

    #[test]
    fn variable_move_state_partial_move_affects_descendants() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_a = interner.get_or_intern("a");
        let field_b = interner.get_or_intern("b");
        let span = Span::new(10, 20);

        // Move s.a
        state.mark_path_moved(&[field_a], span);

        // s.a.b should also be considered moved (parent is moved)
        assert_eq!(state.is_path_moved(&[field_a, field_b]), Some(span));

        // s.b should not be moved
        assert!(state.is_path_moved(&[field_b]).is_none());
    }

    #[test]
    fn variable_move_state_is_path_moved_prefix_cases() {
        // RUE-124: is_path_moved does a single prefix scan over the Vec.
        // Exercise exact, ancestor, sibling, and nested lookups, plus the
        // exact-over-ancestor and shortest-ancestor precedence that only
        // arises once a branch join has unioned nested partials together.
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let c = interner.get_or_intern("c");
        let z = interner.get_or_intern("z");
        let span_a = Span::new(1, 2);
        let span_ab = Span::new(3, 4);

        let mut state = VariableMoveState::default();
        state.mark_path_moved(&[a, b], span_ab);

        // Exact path moved.
        assert_eq!(state.is_path_moved(&[a, b]), Some(span_ab));
        // Descendant of a moved path is moved (ancestor prefix match).
        assert_eq!(state.is_path_moved(&[a, b, c]), Some(span_ab));
        // A shorter prefix that was NOT moved is not moved.
        assert!(state.is_path_moved(&[a]).is_none());
        // Sibling under the same root is not moved.
        assert!(state.is_path_moved(&[a, c]).is_none());
        // Unrelated path is not moved.
        assert!(state.is_path_moved(&[z]).is_none());

        // Now union in the ancestor `s.a` (as a branch join would), so both
        // `[a]` and `[a, b]` are recorded. Precedence must be unchanged from
        // the old exact-then-shortest-ancestor probe order regardless of the
        // Vec's element order:
        state.mark_path_moved(&[a], span_a);
        // Exact match on `[a, b]` still wins over the `[a]` ancestor.
        assert_eq!(state.is_path_moved(&[a, b]), Some(span_ab));
        // For a deeper path with two matching ancestors, the shortest
        // ancestor (`[a]`) is selected.
        assert_eq!(state.is_path_moved(&[a, b, c]), Some(span_a));
        // `[a]` itself is now an exact match.
        assert_eq!(state.is_path_moved(&[a]), Some(span_a));
    }

    #[test]
    fn variable_move_state_multiple_partial_moves() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let field_y = interner.get_or_intern("y");
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        state.mark_path_moved(&[field_x], span1);
        state.mark_path_moved(&[field_y], span2);

        assert!(state.full_move.is_none());
        assert_eq!(state.partial_moves.len(), 2);
        assert_eq!(state.is_path_moved(&[field_x]), Some(span1));
        assert_eq!(state.is_path_moved(&[field_y]), Some(span2));
    }

    #[test]
    fn variable_move_state_full_move_after_partial_clears_partials() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        // First, partially move a field
        state.mark_path_moved(&[field_x], span1);
        assert_eq!(state.partial_moves.len(), 1);

        // Then, fully move the variable
        state.mark_path_moved(&[], span2);

        // Full move should clear partial moves
        assert!(state.full_move.is_some());
        assert!(state.partial_moves.is_empty());
    }

    #[test]
    fn variable_move_state_partial_after_full_is_ignored() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        // First, fully move the variable
        state.mark_path_moved(&[], span1);

        // Then try to partially move a field
        state.mark_path_moved(&[field_x], span2);

        // Partial move should be ignored when already fully moved
        assert_eq!(state.full_move, Some(span1));
        assert!(state.partial_moves.is_empty());
    }

    #[test]
    fn variable_move_state_partial_must_move_tracks_and_reinit_clears() {
        // RUE-186: partial moves are recorded in the MUST set on the
        // straight-line path, and reinitialization clears them from it.
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let elem0 = interner.get_or_intern("0");
        let span = Span::new(10, 20);

        state.mark_path_moved(&[elem0], span);
        assert!(state.partial_moves_on_all_paths.contains(&vec![elem0]));

        state.mark_path_reinitialized(&[elem0]);
        assert!(!state.partial_moves_on_all_paths.contains(&vec![elem0]));
        assert!(state.partial_moves.is_empty());
    }

    #[test]
    fn variable_move_state_merge_intersects_partial_must_moves() {
        // RUE-186: a path moved in only one branch survives in the MAY set
        // (union) but not the MUST set (intersection).
        let interner = ThreadedRodeo::new();
        let elem0 = interner.get_or_intern("0");
        let elem1 = interner.get_or_intern("1");
        let span = Span::new(10, 20);

        let mut b1 = VariableMoveState::default();
        b1.mark_path_moved(&[elem0], span);
        b1.mark_path_moved(&[elem1], span);
        let mut b2 = VariableMoveState::default();
        b2.mark_path_moved(&[elem0], span);

        let merged = VariableMoveState::merge_union(&b1, &b2);
        assert!(merged.partial_moves.iter().any(|(p, _)| p == &vec![elem0]));
        assert!(merged.partial_moves.iter().any(|(p, _)| p == &vec![elem1]));
        assert!(merged.partial_moves_on_all_paths.contains(&vec![elem0]));
        assert!(!merged.partial_moves_on_all_paths.contains(&vec![elem1]));
    }

    #[test]
    fn variable_move_state_merge_full_move_branch_covers_partial_must() {
        // RUE-186: a branch that fully moved the value on all its paths
        // covers every path, so the other branch's per-path MUST set
        // survives the join (whole-in-then / element-wise-in-else).
        let interner = ThreadedRodeo::new();
        let elem0 = interner.get_or_intern("0");
        let span = Span::new(10, 20);

        let mut whole = VariableMoveState::default();
        whole.mark_path_moved(&[], span);
        let mut elementwise = VariableMoveState::default();
        elementwise.mark_path_moved(&[elem0], span);

        let merged = VariableMoveState::merge_union(&whole, &elementwise);
        assert!(!merged.full_move_on_all_paths);
        assert!(merged.partial_moves_on_all_paths.contains(&vec![elem0]));

        // Symmetric.
        let merged = VariableMoveState::merge_union(&elementwise, &whole);
        assert!(merged.partial_moves_on_all_paths.contains(&vec![elem0]));
    }

    #[test]
    fn variable_move_state_is_any_part_moved() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        // Initially nothing is moved
        assert!(state.is_any_part_moved().is_none());

        // After partial move
        state.mark_path_moved(&[field_x], span1);
        assert_eq!(state.is_any_part_moved(), Some(span1));

        // After full move
        let mut state2 = VariableMoveState::default();
        state2.mark_path_moved(&[], span2);
        assert_eq!(state2.is_any_part_moved(), Some(span2));
    }

    #[test]
    fn variable_move_state_merge_union_both_empty() {
        let state1 = VariableMoveState::default();
        let state2 = VariableMoveState::default();

        let merged = VariableMoveState::merge_union(&state1, &state2);

        assert!(merged.is_empty());
    }

    #[test]
    fn variable_move_state_merge_union_one_full_move() {
        let mut state1 = VariableMoveState::default();
        let state2 = VariableMoveState::default();
        let span = Span::new(10, 20);

        state1.mark_path_moved(&[], span);

        let merged = VariableMoveState::merge_union(&state1, &state2);
        assert_eq!(merged.full_move, Some(span));

        // Test other order
        let merged2 = VariableMoveState::merge_union(&state2, &state1);
        assert_eq!(merged2.full_move, Some(span));
    }

    #[test]
    fn variable_move_state_merge_union_both_full_moves_prefers_first() {
        let mut state1 = VariableMoveState::default();
        let mut state2 = VariableMoveState::default();
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        state1.mark_path_moved(&[], span1);
        state2.mark_path_moved(&[], span2);

        let merged = VariableMoveState::merge_union(&state1, &state2);
        assert_eq!(merged.full_move, Some(span1)); // Prefers first
    }

    #[test]
    fn variable_move_state_merge_union_partial_moves() {
        let mut state1 = VariableMoveState::default();
        let mut state2 = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let field_y = interner.get_or_intern("y");
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        state1.mark_path_moved(&[field_x], span1);
        state2.mark_path_moved(&[field_y], span2);

        let merged = VariableMoveState::merge_union(&state1, &state2);

        // Both partial moves should be present
        assert_eq!(merged.partial_moves.len(), 2);
        assert_eq!(merged.is_path_moved(&[field_x]), Some(span1));
        assert_eq!(merged.is_path_moved(&[field_y]), Some(span2));
    }

    #[test]
    fn variable_move_state_merge_union_same_partial_move_prefers_first() {
        let mut state1 = VariableMoveState::default();
        let mut state2 = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span1 = Span::new(10, 20);
        let span2 = Span::new(30, 40);

        state1.mark_path_moved(&[field_x], span1);
        state2.mark_path_moved(&[field_x], span2);

        let merged = VariableMoveState::merge_union(&state1, &state2);

        // Should have the span from the first state
        assert_eq!(merged.partial_moves.len(), 1);
        assert_eq!(merged.is_path_moved(&[field_x]), Some(span1));
    }

    #[test]
    fn full_move_on_all_paths_intersects_at_merge() {
        let span = Span::new(10, 20);

        // Moved in both branches: still moved on all paths.
        let mut both1 = VariableMoveState::default();
        let mut both2 = VariableMoveState::default();
        both1.mark_path_moved(&[], span);
        both2.mark_path_moved(&[], span);
        assert!(both1.full_move_on_all_paths);
        let merged = VariableMoveState::merge_union(&both1, &both2);
        assert!(merged.full_move_on_all_paths);

        // Moved in only one branch: may-move stays (full_move set), but
        // must-move is intersected away.
        let mut one = VariableMoveState::default();
        one.mark_path_moved(&[], span);
        let merged = VariableMoveState::merge_union(&one, &VariableMoveState::default());
        assert_eq!(merged.full_move, Some(span));
        assert!(!merged.full_move_on_all_paths);
    }

    #[test]
    fn union_move_maps_clears_all_paths_for_one_sided_entries() {
        let interner = ThreadedRodeo::new();
        let var = interner.get_or_intern("m");
        let span = Span::new(10, 20);

        let mut then_state = VariableMoveState::default();
        then_state.mark_path_moved(&[], span);
        let mut then_moves = AHashMap::new();
        then_moves.insert(var, then_state);
        let else_moves = AHashMap::new();

        let merged = union_move_maps(&then_moves, &else_moves);
        let state = merged.get(&var).expect("var should be may-moved");
        assert_eq!(state.full_move, Some(span));
        assert!(!state.full_move_on_all_paths);
    }

    #[test]
    fn mark_path_reinitialized_clears_path_and_subpaths_only() {
        let interner = ThreadedRodeo::new();
        let field_f = interner.get_or_intern("f");
        let field_x = interner.get_or_intern("x");
        let field_g = interner.get_or_intern("g");
        let span = Span::new(10, 20);

        let mut state = VariableMoveState::default();
        state.mark_path_moved(&[field_f], span);
        state.mark_path_moved(&[field_f, field_x], span);
        state.mark_path_moved(&[field_g], span);

        state.mark_path_reinitialized(&[field_f]);

        // f and f.x are reinitialized; sibling g stays moved.
        assert!(state.is_path_moved(&[field_f]).is_none());
        assert!(state.is_path_moved(&[field_f, field_x]).is_none());
        assert_eq!(state.is_path_moved(&[field_g]), Some(span));
    }

    #[test]
    fn whole_value_use_rejects_moved_descendants() {
        let s = syms(&["a", "b", "c"]);
        let (a, b, c) = (s[0], s[1], s[2]);
        let mut state = VariableMoveState::default();
        state.mark_path_moved(&[a, b], Span::new(1, 2));

        // Reading a sibling leaf is fine, but using the ancestor as a WHOLE
        // value after a descendant moved out is rejected (spec 3.8, RUE-279).
        assert_eq!(state.is_path_moved(&[a, c]), None);
        assert_eq!(state.is_path_moved(&[a]), None);
        assert_eq!(
            state.is_path_or_descendant_moved(&[a]),
            Some(Span::new(1, 2))
        );
        assert_eq!(state.is_path_or_descendant_moved(&[a, c]), None);
    }

    #[test]
    fn move_state_equality_ignores_partial_move_order() {
        // The loop back-edge recheck compares states for equality, and the
        // partial-move association list is an unordered map (RUE-124).
        let s = syms(&["a", "b"]);
        let (a, b) = (s[0], s[1]);
        let mut forward = VariableMoveState::default();
        forward.mark_path_moved(&[a], Span::new(1, 2));
        forward.mark_path_moved(&[b], Span::new(3, 4));
        let mut reverse = VariableMoveState::default();
        reverse.mark_path_moved(&[b], Span::new(3, 4));
        reverse.mark_path_moved(&[a], Span::new(1, 2));
        assert_eq!(forward, reverse);
        reverse.mark_path_reinitialized(&[b]);
        assert_ne!(forward, reverse);
    }

    #[test]
    fn scope_shadowing_saves_and_restores_move_state() {
        let s = syms(&["x"]);
        let x = s[0];
        let mut state = OwnershipState::default();
        state.push_scope_frame();

        // Move the outer binding, then shadow it in an inner scope: the
        // fresh binding starts unmoved, and the outer state is saved.
        state
            .moved_vars
            .entry(x)
            .or_default()
            .mark_path_moved(&[], Span::new(1, 2));
        state.push_scope_frame();
        state.bind_fresh(x);
        assert!(!state.moved_vars.contains_key(&x));

        // Move the shadow; popping its scope must restore the OUTER
        // binding's move (RUE-522) — not resurrect it, not keep the
        // shadow's span.
        state
            .moved_vars
            .entry(x)
            .or_default()
            .mark_path_moved(&[], Span::new(3, 4));
        state.pop_scope_frame();
        assert_eq!(
            state.moved_vars.get(&x).and_then(|m| m.full_move),
            Some(Span::new(1, 2))
        );
    }

    #[test]
    fn loop_edge_snapshots_replay_scope_restoration_and_merge() {
        let s = syms(&["outer", "inner"]);
        let (outer, inner) = (s[0], s[1]);
        let mut state = OwnershipState::default();

        // Enter the loop's scope and record its edge frame index.
        state.push_scope_frame();
        state
            .loop_break_stack
            .push(LoopEdgeStates::entered_at(state.moved_scope_stack.len()));

        // Inside a nested scope, declare a loop-local `inner` and move both
        // names, then break: the snapshot names both bindings.
        state.push_scope_frame();
        state.bind_fresh(inner);
        state
            .moved_vars
            .entry(outer)
            .or_default()
            .mark_path_moved(&[], Span::new(1, 2));
        state
            .moved_vars
            .entry(inner)
            .or_default()
            .mark_path_moved(&[], Span::new(3, 4));
        let depth = state.moved_scope_stack.len();
        let moves = state.moved_vars.clone();
        state
            .loop_break_stack
            .last_mut()
            .unwrap()
            .record_break(&moves, depth);

        // Popping the nested scope replays its restoration onto the
        // snapshot (RUE-1293): the loop-local drops out, the outer move
        // stays, and the depth tag lowers.
        state.pop_scope_frame();
        let record = state.loop_break_stack.pop().unwrap();
        assert!(record.broke);
        let (break_moves, continue_moves) = record.merged_moves();
        assert!(continue_moves.is_none());
        let break_moves = break_moves.unwrap();
        assert!(!break_moves.contains_key(&inner));
        assert_eq!(
            break_moves.get(&outer).and_then(|m| m.full_move),
            Some(Span::new(1, 2))
        );
    }

    #[test]
    fn expression_ledgers_checkpoint_nest_and_readmit() {
        let s = syms(&["r"]);
        let r = s[0];
        let mut state = OwnershipState::default();

        // Rollback discards records made after the checkpoint.
        state.expression_shared_reads.push((r, Span::new(1, 2)));
        let checkpoint = state.checkpoint_expression_ledgers();
        state
            .expression_loans
            .push((r, Span::new(3, 4), CallLoanKind::Inout));
        state.expression_exclusive_uses.push((r, Span::new(5, 6)));
        state.rollback_expression_ledgers(checkpoint);
        assert!(state.expression_loans.is_empty());
        assert!(state.expression_exclusive_uses.is_empty());
        assert_eq!(state.expression_shared_reads.len(), 1);

        // A nested full expression keeps enclosing loans visible, isolates
        // completed reads, and its readmitted tail loans survive the
        // boundary (RUE-1678).
        state
            .expression_loans
            .push((r, Span::new(1, 2), CallLoanKind::Borrow));
        let boundary = state.enter_full_expression();
        assert_eq!(state.expression_loans.len(), 1);
        assert!(state.expression_shared_reads.is_empty());
        state
            .expression_loans
            .push((r, Span::new(7, 8), CallLoanKind::Inout));
        let tail_loans = state.nested_expression_loans(&boundary);
        state.exit_full_expression(boundary);
        assert_eq!(state.expression_loans.len(), 1);
        assert_eq!(state.expression_shared_reads.len(), 1);
        state.readmit_expression_loans(tail_loans);
        assert_eq!(state.expression_loans.len(), 2);
    }

    #[test]
    fn recheck_fork_carries_moves_and_clears_operand_markers() {
        let s = syms(&["x"]);
        let x = s[0];
        let mut state = OwnershipState::default();
        state
            .moved_vars
            .entry(x)
            .or_default()
            .mark_path_moved(&[], Span::new(1, 2));
        state.byref_arg_root = Some(x);
        state.drop_intrinsic_operand = Some(x);
        state
            .call_loaned_roots
            .push(vec![(x, CallLoanKind::Inout, false)]);

        let fork = state.fork_for_recheck();
        assert!(fork.in_loop_move_recheck);
        assert!(fork.byref_arg_root.is_none());
        assert!(fork.drop_intrinsic_operand.is_none());
        assert_eq!(fork.moved_vars, state.moved_vars);
        assert_eq!(fork.call_loaned_roots.len(), 1);
    }

    #[test]
    fn merge_branch_moves_uses_the_surviving_branch() {
        let s = syms(&["x", "y"]);
        let (x, y) = (s[0], s[1]);
        let mut then_moves = AHashMap::new();
        let mut x_state = VariableMoveState::default();
        x_state.mark_path_moved(&[], Span::new(1, 2));
        then_moves.insert(x, x_state);
        let mut else_moves = AHashMap::new();
        let mut y_state = VariableMoveState::default();
        y_state.mark_path_moved(&[], Span::new(3, 4));
        else_moves.insert(y, y_state);

        // Then-branch diverges: only the else-branch's moves survive.
        let mut state = OwnershipState::default();
        state.merge_branch_moves(
            then_moves.clone(),
            else_moves.clone(),
            true,
            false,
            DivergenceKinds::OTHER,
            DivergenceKinds::NONE,
        );
        assert!(!state.moved_vars.contains_key(&x));
        assert!(state.moved_vars.contains_key(&y));

        // Neither diverges: union, and neither move holds on all paths.
        let mut state = OwnershipState::default();
        state.merge_branch_moves(
            then_moves,
            else_moves,
            false,
            false,
            DivergenceKinds::NONE,
            DivergenceKinds::NONE,
        );
        assert!(state.moved_vars.contains_key(&x));
        assert!(state.moved_vars.contains_key(&y));
        assert!(!state.moved_vars[&x].full_move_on_all_paths);
    }
}
