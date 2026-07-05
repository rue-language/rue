//! Analysis context and helper types for semantic analysis.
//!
//! This module contains the supporting structures used during function body
//! analysis, including local variable tracking, scope management, and move
//! state tracking for affine types.

use std::collections::{HashMap, HashSet};

use lasso::Spur;
use rue_builtins::BuiltinTypeDef;
use rue_error::CompileWarning;
use rue_rir::RirParamMode;
use rue_span::Span;

use crate::scope::ScopedContext;
use crate::types::{StructId, Type};

/// Information about a local variable.
#[derive(Debug, Clone)]
pub(crate) struct LocalVar {
    /// Slot index for this variable
    pub slot: u32,
    /// Type of the variable
    pub ty: Type,
    /// Whether the variable is mutable
    pub is_mut: bool,
    /// Span of the variable declaration (for unused variable warnings)
    pub span: Span,
    /// Whether @allow(unused_variable) was applied to this binding
    pub allow_unused: bool,
}

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
    pub partial_moves_on_all_paths: HashSet<FieldPath>,
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

/// Union-merge two branch move-state maps (see [`VariableMoveState::merge_union`]).
///
/// A variable with state in only one map is merged against the default
/// (no-moves) state, which correctly clears `full_move_on_all_paths`: the
/// other branch did not move it.
pub(crate) fn union_move_maps(
    branch1: &HashMap<Spur, VariableMoveState>,
    branch2: &HashMap<Spur, VariableMoveState>,
) -> HashMap<Spur, VariableMoveState> {
    let default = VariableMoveState::default();
    let mut merged = HashMap::new();
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

/// Information about a function parameter.
#[derive(Debug, Clone)]
pub(crate) struct ParamInfo {
    /// Parameter name symbol
    pub name: Spur,
    /// Starting ABI slot for this parameter (0-based).
    /// For scalar types, this is the single slot.
    /// For struct types, this is the first field's slot.
    pub abi_slot: u32,
    /// Parameter type
    pub ty: Type,
    /// Parameter passing mode
    pub mode: RirParamMode,
    /// Whether the parameter is declared `comptime` (carried separately from
    /// `mode`, which stays `Normal` for comptime parameters — see
    /// `RirParam::is_comptime`). Used to allow forwarding a comptime
    /// parameter to another function's comptime parameter (spec 4.14:5).
    pub is_comptime: bool,
}

/// Context for analyzing instructions within a function.
///
/// Bundles together the mutable state that needs to be threaded through
/// recursive `analyze_inst` calls.
pub(crate) struct AnalysisContext<'a> {
    /// Local variables in scope
    pub locals: HashMap<Spur, LocalVar>,
    /// Function parameters (immutable reference, shared across the function)
    pub params: &'a [ParamInfo],
    /// Next available slot for local variables
    pub next_slot: u32,
    /// How many loops we're nested inside (for break/continue validation)
    pub loop_depth: u32,
    /// How many `checked` blocks we're nested inside. Unchecked operations —
    /// raw-pointer intrinsics (`@raw`, `@ptr_read`, …) and calls to `unchecked
    /// fn`s — are only legal when this is greater than zero (spec 9.1:1,
    /// chapter 9). An `unchecked fn` body does NOT implicitly count as a
    /// checked context; the modifier only gates *callers* (see spec 9.1:1).
    pub checked_depth: u32,
    /// One entry per enclosing loop (innermost last); set to `true` when a
    /// `break` targeting that loop is analyzed. An infinite loop containing a
    /// break has type `()`; without one it has type `!` (see spec 4.8).
    pub loop_break_stack: Vec<bool>,
    /// Local variables that have been read (for unused variable detection)
    pub used_locals: HashSet<Spur>,
    /// Return type of the current function (for explicit return validation)
    pub return_type: Type,
    /// Scope stack for efficient scope management.
    /// Each entry is a list of (symbol, old_value) pairs for variables added/shadowed in that scope.
    /// When a scope is popped, we restore old values (for shadowed vars) or remove new vars.
    pub scope_stack: Vec<Vec<(Spur, Option<LocalVar>)>>,
    /// Resolved types from HM inference.
    /// Maps RIR instruction refs to their resolved concrete types.
    /// This is populated by running constraint generation and unification
    /// before AIR emission.
    pub resolved_types: &'a HashMap<InstRef, Type>,
    /// Variables that have been moved (for affine type checking).
    /// Maps variable symbol to move state (supports partial/field-level moves).
    pub moved_vars: HashMap<Spur, VariableMoveState>,
    /// Warnings collected during this function's analysis.
    /// Finalization merges these per-function warnings into the global output.
    pub warnings: Vec<CompileWarning>,
    /// Whether the current function carries `@allow(unused_variable)`.
    /// When set, every local unused-variable warning in this body is
    /// suppressed (spec 2.5:17).
    pub allow_unused_variables: bool,
    /// Local string table: maps string content to local index (for deduplication within function).
    /// Finalization merges these strings globally after body analysis.
    pub local_string_table: HashMap<String, u32>,
    /// Local string data indexed by local string table index.
    /// After analysis, these are merged into the global string table with ID remapping.
    pub local_strings: Vec<String>,
    /// Comptime type variables: maps variable symbols to their compile-time type values.
    /// When a variable is bound to a comptime type (e.g., `let P = make_point()` where
    /// `make_point() -> type`), this map stores the resolved type so it can be used
    /// as a type annotation (e.g., `let p: P = ...`).
    pub comptime_type_vars: HashMap<Spur, Type>,
    /// Comptime value variables: maps variable symbols to their compile-time constant values.
    /// When an anonymous struct method captures comptime parameters from the enclosing function
    /// (e.g., `fn FixedBuffer(comptime N: i32)` creates a struct with methods that reference `N`),
    /// this map stores the captured values so method bodies can resolve them.
    pub comptime_value_vars: HashMap<Spur, ConstValue>,
    /// Functions referenced during analysis of this function.
    /// Used for lazy semantic analysis (Phase 3 of module system) to track
    /// which functions need to be analyzed. Each entry is a function name symbol.
    pub referenced_functions: HashSet<Spur>,
    /// Methods referenced during analysis of this function.
    /// Each entry is (struct_id, method_name) matching the key format in methods map.
    pub referenced_methods: HashSet<(StructId, Spur)>,
    /// While analyzing the value of an `inout`/`borrow` call argument, the ROOT
    /// variable of the place being passed by reference (set by
    /// `analyze_call_args`; for `f(borrow o.f)` this is `o`). A by-ref argument
    /// is a borrow, not a move, so the variable-reference and place analyses
    /// must not mark the root (or the projected path) as moved — and must not
    /// reject forwarding a by-ref parameter to another function's by-ref
    /// parameter (RUE-143).
    pub byref_arg_root: Option<Spur>,
    /// True while re-running a loop's condition/body to validate the loop's
    /// back edge (see [`AnalysisContext::fork_for_loop_recheck`]). The recheck
    /// pass starts from a move state that already includes every move the loop
    /// performs, so nested loops analyzed within it don't need (and must not
    /// trigger) their own recheck — that would make nested loops exponential.
    pub in_loop_move_recheck: bool,
    /// Collection variables currently held as a scoped shared borrow by an
    /// enclosing `for` loop (innermost last), spec 4.8:26 / RUE-233. While a
    /// name is here, any mutation of it in the loop body — whole-variable
    /// assignment (`a = …`), field set (`a.f = …`), or element set
    /// (`a[i] = …`) — is rejected with E0428 (`MutateBorrowedValue`), matching
    /// the treatment of an explicit `borrow` parameter. Reads (including the
    /// loop's own element reads) are permitted.
    pub iter_borrows: Vec<Spur>,
    /// The type this expression is expected to produce, when sema knows it from
    /// a surrounding annotation or pattern. Set narrowly around a
    /// let-initializer (to the resolved annotation type) and around a `match`
    /// scrutinee (to the enum named by the arm patterns). The fallible
    /// intrinsics (`@read_line`, `@parse_*`) read this to learn which in-scope
    /// `Option(T)` to return (RUE-6, ADR-0038) — inference cannot supply it
    /// because comptime-generic library enums are resolved only in sema. Left
    /// `None` everywhere else, so no other analysis is affected.
    pub expected_type: Option<Type>,
    /// True only while analyzing the operand of a `?` expression (RUE-318). The
    /// `?` site cannot supply an `expected_type` for a *bare* fallible intrinsic
    /// (`@read_line()?` / `@parse_i64(s)?`): the enclosing function's `Option(U)`
    /// has the wrong payload (e.g. `@read_line` is `Option(String)`, not the
    /// function's `Option(i64)`). When this flag is set and no valid expected
    /// `Option` is available, a fallible intrinsic instantiates its *own* fixed
    /// `Option(payload)` (it knows its payload) via `find_or_create_anon_enum`,
    /// the same canonical library enum an in-scope `Option(T)` would produce.
    /// Left `false` everywhere else, so the plain "context supplies the Option"
    /// path (a `let` annotation or `match` arms) is unaffected.
    pub try_operand: bool,
}

// Import InstRef for use in resolved_types
use rue_rir::InstRef;

impl ScopedContext for AnalysisContext<'_> {
    type VarInfo = LocalVar;

    fn locals_mut(&mut self) -> &mut HashMap<Spur, Self::VarInfo> {
        &mut self.locals
    }

    fn scope_stack_mut(&mut self) -> &mut Vec<Vec<(Spur, Option<Self::VarInfo>)>> {
        &mut self.scope_stack
    }

    /// Insert a local variable, tracking it in the current scope for later cleanup.
    ///
    /// This override also clears any moved state for the variable, which handles
    /// shadowing: `let x = moved_val; let x = new_val;`
    /// The new `x` is a fresh binding and shouldn't carry the old moved state.
    fn insert_local(&mut self, symbol: Spur, var: LocalVar) {
        let old_value = self.locals.insert(symbol, var);
        // Track in the current scope (if any) for cleanup on pop
        if let Some(current_scope) = self.scope_stack.last_mut() {
            current_scope.push((symbol, old_value));
        }
        // When a variable is (re)declared, clear any moved state for it.
        self.moved_vars.remove(&symbol);
    }
}

impl<'a> AnalysisContext<'a> {
    /// Create a scratch copy of this context for the loop back-edge move check.
    ///
    /// A value moved anywhere in a loop's condition or body is already moved
    /// when the back edge re-enters the loop. After analyzing a loop once, the
    /// loop is re-analyzed against a fork of the context (and a scratch `Air`)
    /// whose starting move state is the *post-body* state; any use of a moved
    /// value then surfaces as a `UseAfterMove` error pointing at the move from
    /// the "previous iteration". One re-run reaches a fixpoint: analysis is
    /// deterministic, so re-running from the post-body state marks exactly the
    /// same moves again.
    ///
    /// Output accumulators (warnings, referenced functions/methods) start
    /// empty so the discarded pass doesn't duplicate entries in the real
    /// context.
    pub fn fork_for_loop_recheck(&self) -> AnalysisContext<'a> {
        AnalysisContext {
            locals: self.locals.clone(),
            params: self.params,
            next_slot: self.next_slot,
            loop_depth: self.loop_depth,
            checked_depth: self.checked_depth,
            loop_break_stack: self.loop_break_stack.clone(),
            used_locals: self.used_locals.clone(),
            return_type: self.return_type,
            scope_stack: self.scope_stack.clone(),
            resolved_types: self.resolved_types,
            moved_vars: self.moved_vars.clone(),
            warnings: Vec::new(),
            allow_unused_variables: self.allow_unused_variables,
            local_string_table: self.local_string_table.clone(),
            local_strings: self.local_strings.clone(),
            comptime_type_vars: self.comptime_type_vars.clone(),
            comptime_value_vars: self.comptime_value_vars.clone(),
            referenced_functions: HashSet::new(),
            referenced_methods: HashSet::new(),
            // A by-ref argument's value is a place — a variable or a
            // field/index projection chain (enforced in analyze_call_args) —
            // and index subexpressions are analyzed with the root cleared
            // (see try_trace_place_inner), so a loop can never be analyzed
            // while this is set; the fork starts between whole-expression
            // analyses.
            byref_arg_root: None,
            in_loop_move_recheck: true,
            iter_borrows: self.iter_borrows.clone(),
            expected_type: None,
            try_operand: false,
        }
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
    /// - If both diverge, the whole if-else diverges and moves don't matter
    pub fn merge_branch_moves(
        &mut self,
        then_moves: HashMap<Spur, VariableMoveState>,
        else_moves: HashMap<Spur, VariableMoveState>,
        then_diverges: bool,
        else_diverges: bool,
    ) {
        // If then-branch diverges, use else-branch's moves
        // If else-branch diverges, use then-branch's moves
        // If both diverge, the whole expression diverges - doesn't matter what we do
        // If neither diverges, merge the moves (union - moved in either = moved after)
        match (then_diverges, else_diverges) {
            (true, true) => {
                // Both branches diverge - no need to merge, the code after
                // the if-else is unreachable. Use then_moves arbitrarily.
                self.moved_vars = then_moves;
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
    /// never reach the join, so they are excluded; if every arm diverges the
    /// match itself diverges and any arm's state will do (code after the
    /// match is unreachable).
    ///
    /// Each entry is the `moved_vars` snapshot after analyzing one arm
    /// (starting from the same pre-match state), paired with whether that
    /// arm's body diverges.
    pub fn merge_arm_moves(&mut self, arm_moves: Vec<(HashMap<Spur, VariableMoveState>, bool)>) {
        let mut live = arm_moves
            .iter()
            .filter(|(_, diverges)| !diverges)
            .map(|(moves, _)| moves);

        let Some(first) = live.next() else {
            // Every arm diverges. (A zero-arm match on a zero-variant enum
            // returns early in analyze_match and never reaches this merge.)
            if let Some((moves, _)) = arm_moves.into_iter().next() {
                self.moved_vars = moves;
            }
            return;
        };

        let mut merged = first.clone();
        for arm in live {
            merged = union_move_maps(&merged, arm);
        }
        self.moved_vars = merged;
    }

    /// Add a string to the local string table, returning its local index.
    ///
    /// This deduplicates strings within a single function. After function analysis
    /// completes, local strings are merged into the global string table with ID
    /// remapping in the AIR instructions.
    pub fn add_local_string(&mut self, content: String) -> u32 {
        use std::collections::hash_map::Entry;
        match self.local_string_table.entry(content) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = self.local_strings.len() as u32;
                self.local_strings.push(e.key().clone());
                e.insert(id);
                id
            }
        }
    }
}

/// Result of analyzing an instruction: the AIR reference and its synthesized type.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalysisResult {
    /// Reference to the generated AIR instruction
    pub air_ref: AirRef,
    /// The synthesized type of this expression
    pub ty: Type,
}

use crate::inst::{AirPlaceRef, AirRef};

impl AnalysisResult {
    #[must_use]
    pub fn new(air_ref: AirRef, ty: Type) -> Self {
        Self { air_ref, ty }
    }
}

/// Represents a compile-time constant value.
///
/// This is used for compile-time evaluation of expressions and for
/// comptime parameters. For example, in `fn Buffer(comptime N: i32)`,
/// the value of `N` is stored as a `ConstValue::Integer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstValue {
    /// Integer value. Backed by `i128` so the full range of every Rue integer
    /// type is representable (u64 values above `i64::MAX` as well as negative
    /// signed values). Range checks against the expression's Rue type happen
    /// in the comptime evaluator (see `sema::comptime_eval`).
    Integer(i128),
    /// Boolean value
    Bool(bool),
    /// Type value - stores a concrete type for type parameters.
    /// This is used when a `comptime T: type` parameter is instantiated
    /// with a specific type like `i32` or `bool`.
    Type(Type),
    /// Function reference - stores the callee symbol.
    ///
    /// Function-valued constants are currently callable aliases only: they can
    /// be used as the callee of a call expression, but cannot be materialized
    /// as ordinary runtime values.
    Function(lasso::Spur),
    /// Unit value - the value of `()`.
    // TODO(rue-c6gi): Remove #[allow] when Unit const evaluation is implemented
    #[allow(dead_code)]
    Unit,
}

impl ConstValue {
    /// Try to extract an integer value that fits in an `i64`.
    ///
    /// Returns `None` for non-integer values and for integers outside the
    /// `i64` range (e.g. u64 values above `i64::MAX`). Use [`as_int_value`]
    /// when the full `i128` backing value is needed.
    ///
    /// [`as_int_value`]: ConstValue::as_int_value
    pub fn as_integer(self) -> Option<i64> {
        match self {
            ConstValue::Integer(n) => i64::try_from(n).ok(),
            _ => None,
        }
    }

    /// Try to extract the full backing integer value.
    pub fn as_int_value(self) -> Option<i128> {
        match self {
            ConstValue::Integer(n) => Some(n),
            _ => None,
        }
    }

    /// Try to extract a boolean value.
    pub fn as_bool(self) -> Option<bool> {
        match self {
            ConstValue::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// Try to extract a type value.
    // TODO(rue-c6gi): Remove #[allow] when Unit const evaluation is implemented
    #[allow(dead_code)]
    pub fn as_type(self) -> Option<Type> {
        match self {
            ConstValue::Type(ty) => Some(ty),
            _ => None,
        }
    }

    /// Try to extract a function reference.
    pub fn as_function(self) -> Option<lasso::Spur> {
        match self {
            ConstValue::Function(name) => Some(name),
            _ => None,
        }
    }

    /// Check if this is a unit value.
    // TODO(rue-c6gi): Remove #[allow] when Unit const evaluation is implemented
    #[allow(dead_code)]
    pub fn is_unit(self) -> bool {
        matches!(self, ConstValue::Unit)
    }

    /// Get the type of this constant value.
    // TODO(rue-c6gi): Remove #[allow] when Unit const evaluation is implemented
    #[allow(dead_code)]
    pub fn get_type(&self) -> Type {
        match self {
            ConstValue::Integer(_) => Type::I64, // Default to i64 for comptime integers
            ConstValue::Bool(_) => Type::BOOL,
            ConstValue::Type(_) => Type::COMPTIME_TYPE,
            ConstValue::Function(_) => Type::COMPTIME_TYPE,
            ConstValue::Unit => Type::UNIT,
        }
    }
}

/// Storage location for a String receiver in mutation methods.
///
/// This is used by `analyze_builtin_method` to store the updated
/// String back to the original variable after calling the runtime function.
pub(crate) enum StringReceiverStorage {
    /// The receiver is a local variable with the given slot.
    Local { slot: u32 },
    /// The receiver is a parameter with the given ABI slot.
    Param { abi_slot: u32 },
    /// The receiver is a projection place — a struct field, an array element,
    /// or a chain rooted at a local/parameter (`b.data`, `arr[0]`,
    /// `self.inner`). The mutation result is written back through this place
    /// (RUE-256).
    Place { place: AirPlaceRef },
}

/// Context for analyzing a method call on a builtin type.
///
/// Groups together the parameters that describe which builtin method is being
/// called, reducing the number of parameters to `analyze_builtin_method`.
pub(crate) struct BuiltinMethodContext<'a> {
    /// The struct ID of the builtin type (e.g., String).
    pub struct_id: StructId,
    /// The builtin type definition containing method metadata.
    pub builtin_def: &'static BuiltinTypeDef,
    /// The name of the method being called.
    pub method_name: &'a str,
    /// The source span for error reporting.
    pub span: Span,
}

/// Information about the receiver of a method call.
///
/// Groups together the receiver-related parameters for `analyze_builtin_method`,
/// including the analyzed receiver expression, the original variable (if any),
/// and the storage location for mutation methods.
pub(crate) struct ReceiverInfo {
    /// The analysis result of the receiver expression.
    pub result: AnalysisResult,
    /// The root variable symbol if the receiver is a variable reference.
    /// Used to track moves and "unmove" for borrow semantics.
    pub var: Option<Spur>,
    /// The move state of `var` captured BEFORE the receiver expression was
    /// analyzed (`None` = the variable had no move state). ByRef/ByMutRef
    /// methods restore this snapshot to undo exactly the move the receiver
    /// analysis recorded — a whole-map `remove` would also erase earlier,
    /// unrelated moves of sibling fields (`consume(w.s); w.t.len()` must not
    /// forget that `w.s` is moved — RUE-33).
    pub move_state_before: Option<VariableMoveState>,
    /// Storage location for mutation methods that need to write back.
    /// Only set when the receiver is a mutable lvalue and the method mutates.
    pub storage: Option<StringReceiverStorage>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    // =========================================================================
    // VariableMoveState tests
    // =========================================================================

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
        let mut then_moves = HashMap::new();
        then_moves.insert(var, then_state);
        let else_moves = HashMap::new();

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

    // =========================================================================
    // ConstValue tests
    // =========================================================================

    #[test]
    fn const_value_as_integer() {
        let cv = ConstValue::Integer(42);
        assert_eq!(cv.as_integer(), Some(42));
        assert_eq!(cv.as_bool(), None);
    }

    #[test]
    fn const_value_as_bool() {
        let cv = ConstValue::Bool(true);
        assert_eq!(cv.as_bool(), Some(true));
        assert_eq!(cv.as_integer(), None);

        let cv2 = ConstValue::Bool(false);
        assert_eq!(cv2.as_bool(), Some(false));
    }

    #[test]
    fn const_value_negative_integer() {
        let cv = ConstValue::Integer(-100);
        assert_eq!(cv.as_integer(), Some(-100));
    }

    #[test]
    fn const_value_equality() {
        assert_eq!(ConstValue::Integer(42), ConstValue::Integer(42));
        assert_ne!(ConstValue::Integer(42), ConstValue::Integer(43));
        assert_eq!(ConstValue::Bool(true), ConstValue::Bool(true));
        assert_ne!(ConstValue::Bool(true), ConstValue::Bool(false));
        assert_ne!(ConstValue::Integer(1), ConstValue::Bool(true));
    }

    #[test]
    fn const_value_as_type() {
        let cv = ConstValue::Type(Type::I32);
        assert_eq!(cv.as_type(), Some(Type::I32));
        assert_eq!(cv.as_integer(), None);
        assert_eq!(cv.as_bool(), None);

        let cv2 = ConstValue::Type(Type::BOOL);
        assert_eq!(cv2.as_type(), Some(Type::BOOL));
    }

    #[test]
    fn const_value_unit() {
        let cv = ConstValue::Unit;
        assert!(cv.is_unit());
        assert_eq!(cv.as_integer(), None);
        assert_eq!(cv.as_bool(), None);
        assert_eq!(cv.as_type(), None);
    }

    #[test]
    fn const_value_get_type() {
        assert_eq!(ConstValue::Integer(42).get_type(), Type::I64);
        assert_eq!(ConstValue::Bool(true).get_type(), Type::BOOL);
        assert_eq!(ConstValue::Type(Type::I32).get_type(), Type::COMPTIME_TYPE);
        assert_eq!(ConstValue::Unit.get_type(), Type::UNIT);
    }

    #[test]
    fn const_value_type_equality() {
        assert_eq!(ConstValue::Type(Type::I32), ConstValue::Type(Type::I32));
        assert_ne!(ConstValue::Type(Type::I32), ConstValue::Type(Type::I64));
        assert_ne!(ConstValue::Type(Type::I32), ConstValue::Integer(32));
    }

    // =========================================================================
    // AnalysisResult tests
    // =========================================================================

    #[test]
    fn analysis_result_new() {
        let air_ref = AirRef::from_raw(5);
        let ty = Type::I32;

        let result = AnalysisResult::new(air_ref, ty);

        assert_eq!(result.air_ref.as_u32(), 5);
        assert_eq!(result.ty, Type::I32);
    }
}
