//! Analysis context and helper types for semantic analysis.
//!
//! This module contains the supporting structures used during function body
//! analysis, including local variable tracking, scope management, and move
//! state tracking for affine types.

use std::collections::{HashMap, HashSet};

use lasso::Spur;
use rue_error::CompileWarning;
use rue_rir::RirParamMode;
use rue_span::Span;

use crate::types::Type;

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
pub(crate) type FieldPath = Vec<Spur>;

/// Information about a variable that has been moved.
#[derive(Debug, Clone)]
pub(crate) struct MoveInfo {
    /// Span where the move occurred
    pub moved_at: Span,
}

/// Tracks move state for a variable, including partial (field-level) moves.
#[derive(Debug, Clone, Default)]
pub(crate) struct VariableMoveState {
    /// If Some, the entire variable has been fully moved at this span.
    pub full_move: Option<Span>,
    /// Partial moves: maps field paths to the span where they were moved.
    /// For example, if `s.a` was moved, this contains ([sym("a")], span).
    pub partial_moves: HashMap<FieldPath, Span>,
}

impl VariableMoveState {
    /// Mark a field path as moved.
    pub fn mark_path_moved(&mut self, path: &[Spur], span: Span) {
        if path.is_empty() {
            // Moving the whole variable
            self.full_move = Some(span);
            // Clear partial moves since the whole thing is moved
            self.partial_moves.clear();
        } else {
            // Partial move - only if not already fully moved
            if self.full_move.is_none() {
                self.partial_moves.insert(path.to_vec(), span);
            }
        }
    }

    /// Check if a field path is moved.
    /// Returns Some(span) if the path (or any ancestor) is moved.
    pub fn is_path_moved(&self, path: &[Spur]) -> Option<Span> {
        // If fully moved, everything is moved
        if let Some(span) = self.full_move {
            return Some(span);
        }

        // Check if this exact path is moved
        if let Some(span) = self.partial_moves.get(path) {
            return Some(*span);
        }

        // Check if any prefix (ancestor) of this path is moved
        // e.g., if s.a is moved, then s.a.b is also considered moved
        for len in 1..path.len() {
            if let Some(span) = self.partial_moves.get(&path[..len]) {
                return Some(*span);
            }
        }

        None
    }

    /// Check if the variable is partially moved (some field is moved but not the whole var).
    /// Returns Some(span) of the first partial move found.
    pub fn is_partially_moved(&self) -> Option<Span> {
        if self.full_move.is_some() {
            return None; // Fully moved, not partially moved
        }
        self.partial_moves.values().next().copied()
    }

    /// Check if the entire variable (including all fields) is fully valid to use.
    /// Returns Some(span) if there's any move (full or partial) that would prevent use.
    pub fn is_any_part_moved(&self) -> Option<Span> {
        if let Some(span) = self.full_move {
            return Some(span);
        }
        self.partial_moves.values().next().copied()
    }

    /// Clear all move state (used when variable is reassigned).
    pub fn clear(&mut self) {
        self.full_move = None;
        self.partial_moves.clear();
    }

    /// Check if the variable has any move state.
    pub fn is_empty(&self) -> bool {
        self.full_move.is_none() && self.partial_moves.is_empty()
    }

    /// Merge move states from two branches (union semantics).
    /// A variable is considered moved after a branch if it was moved in EITHER branch.
    /// This prevents use-after-move when a value might have been moved.
    pub fn merge_union(branch1: &Self, branch2: &Self) -> Self {
        // If either branch has a full move, the result is a full move
        // (use the span from whichever branch has it, preferring branch1)
        let full_move = branch1.full_move.or(branch2.full_move);

        // A partial move is kept if it appears in EITHER branch
        let mut partial_moves = branch1.partial_moves.clone();
        for (path, span) in &branch2.partial_moves {
            partial_moves.entry(path.clone()).or_insert(*span);
        }

        Self {
            full_move,
            partial_moves,
        }
    }
}

/// Information about a function parameter.
#[derive(Debug, Clone)]
pub(crate) struct ParamInfo {
    /// Starting ABI slot for this parameter (0-based).
    /// For scalar types, this is the single slot.
    /// For struct types, this is the first field's slot.
    pub abi_slot: u32,
    /// Parameter type
    pub ty: Type,
    /// Parameter passing mode
    pub mode: RirParamMode,
}

/// Context for analyzing instructions within a function.
///
/// Bundles together the mutable state that needs to be threaded through
/// recursive `analyze_inst` calls.
pub(crate) struct AnalysisContext<'a> {
    /// Local variables in scope
    pub locals: HashMap<Spur, LocalVar>,
    /// Function parameters (immutable reference, shared across the function)
    pub params: &'a HashMap<Spur, ParamInfo>,
    /// Next available slot for local variables
    pub next_slot: u32,
    /// How many loops we're nested inside (for break/continue validation)
    pub loop_depth: u32,
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
    /// Warnings collected during function analysis.
    /// This is per-function to enable future parallel analysis.
    pub warnings: Vec<CompileWarning>,
}

// Import InstRef for use in resolved_types
use rue_rir::InstRef;

impl AnalysisContext<'_> {
    /// Push a new scope onto the stack.
    pub fn push_scope(&mut self) {
        // Preallocate for a small number of variables. Most scopes (loop bodies,
        // if/match arms) have 0-2 variables; function bodies have more but are
        // less frequent. 2 is a conservative choice until we have real metrics.
        self.scope_stack.push(Vec::with_capacity(2));
    }

    /// Pop the current scope, restoring any shadowed variables and removing new ones.
    pub fn pop_scope(&mut self) {
        if let Some(scope_entries) = self.scope_stack.pop() {
            for (symbol, old_value) in scope_entries {
                match old_value {
                    Some(old_var) => {
                        // Restore the shadowed variable
                        self.locals.insert(symbol, old_var);
                    }
                    None => {
                        // Remove the variable that was added in this scope
                        self.locals.remove(&symbol);
                    }
                }
            }
        }
    }

    /// Insert a local variable, tracking it in the current scope for later cleanup.
    pub fn insert_local(&mut self, symbol: Spur, var: LocalVar) {
        let old_value = self.locals.insert(symbol, var);
        // Track in the current scope (if any) for cleanup on pop
        if let Some(current_scope) = self.scope_stack.last_mut() {
            current_scope.push((symbol, old_value));
        }
        // When a variable is (re)declared, clear any moved state for it.
        // This handles shadowing: `let x = moved_val; let x = new_val;`
        // The new `x` is a fresh binding and shouldn't carry the old moved state.
        self.moved_vars.remove(&symbol);
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
                // A variable is moved after if-else if moved in EITHER branch.
                let mut merged = HashMap::new();

                // Include all moves from then-branch
                for (symbol, then_state) in &then_moves {
                    if let Some(else_state) = else_moves.get(symbol) {
                        // Variable has state in both branches - merge them
                        let merged_state = VariableMoveState::merge_union(then_state, else_state);
                        if !merged_state.is_empty() {
                            merged.insert(*symbol, merged_state);
                        }
                    } else {
                        // Variable only moved in then-branch
                        if !then_state.is_empty() {
                            merged.insert(*symbol, then_state.clone());
                        }
                    }
                }

                // Include moves that only appear in else-branch
                for (symbol, else_state) in &else_moves {
                    if !then_moves.contains_key(symbol) && !else_state.is_empty() {
                        merged.insert(*symbol, else_state.clone());
                    }
                }

                self.moved_vars = merged;
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

use crate::inst::AirRef;

impl AnalysisResult {
    #[must_use]
    pub fn new(air_ref: AirRef, ty: Type) -> Self {
        Self { air_ref, ty }
    }
}

/// A value that can be computed at compile time.
///
/// This is used for constant expression evaluation, primarily for compile-time
/// bounds checking. It can be extended for future `comptime` features.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstValue {
    /// Integer value (signed to handle arithmetic correctly)
    Integer(i64),
    /// Boolean value
    Bool(bool),
}

impl ConstValue {
    /// Try to extract an integer value.
    pub fn as_integer(self) -> Option<i64> {
        match self {
            ConstValue::Integer(n) => Some(n),
            ConstValue::Bool(_) => None,
        }
    }

    /// Try to extract a boolean value.
    pub fn as_bool(self) -> Option<bool> {
        match self {
            ConstValue::Bool(b) => Some(b),
            ConstValue::Integer(_) => None,
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
}
