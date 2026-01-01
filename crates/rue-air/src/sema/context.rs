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
    /// Local string table: maps string content to local index (for deduplication within function).
    /// This is per-function to enable parallel analysis - strings are merged globally after.
    pub local_string_table: HashMap<String, u32>,
    /// Local string data indexed by local string table index.
    /// After analysis, these are merged into the global string table with ID remapping.
    pub local_strings: Vec<String>,
}

// Import InstRef for use in resolved_types
use rue_rir::InstRef;

impl ScopedContext for AnalysisContext<'_> {
    type VarInfo = LocalVar;

    fn locals(&self) -> &HashMap<Spur, Self::VarInfo> {
        &self.locals
    }

    fn locals_mut(&mut self) -> &mut HashMap<Spur, Self::VarInfo> {
        &mut self.locals
    }

    fn scope_stack(&self) -> &[Vec<(Spur, Option<Self::VarInfo>)>] {
        &self.scope_stack
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

impl AnalysisContext<'_> {
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
    /// Type value - stores a concrete type for type parameters.
    /// This is used when a `comptime T: type` parameter is instantiated
    /// with a specific type like `i32` or `bool`.
    Type(Type),
    /// Unit value - the value of `()`.
    Unit,
}

impl ConstValue {
    /// Try to extract an integer value.
    pub fn as_integer(self) -> Option<i64> {
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
    pub fn as_type(self) -> Option<Type> {
        match self {
            ConstValue::Type(ty) => Some(ty),
            _ => None,
        }
    }

    /// Check if this is a unit value.
    pub fn is_unit(self) -> bool {
        matches!(self, ConstValue::Unit)
    }

    /// Get the type of this constant value.
    pub fn get_type(&self) -> Type {
        match self {
            ConstValue::Integer(_) => Type::I64, // Default to i64 for comptime integers
            ConstValue::Bool(_) => Type::Bool,
            ConstValue::Type(_) => Type::ComptimeType,
            ConstValue::Unit => Type::Unit,
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
    fn variable_move_state_is_partially_moved() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span = Span::new(10, 20);

        // Initially not partially moved
        assert!(state.is_partially_moved().is_none());

        // After partial move
        state.mark_path_moved(&[field_x], span);
        assert_eq!(state.is_partially_moved(), Some(span));

        // After full move, is_partially_moved returns None
        state.mark_path_moved(&[], Span::new(50, 60));
        assert!(state.is_partially_moved().is_none());
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
    fn variable_move_state_clear() {
        let mut state = VariableMoveState::default();
        let interner = ThreadedRodeo::new();
        let field_x = interner.get_or_intern("x");
        let span = Span::new(10, 20);

        state.mark_path_moved(&[], span);
        state.partial_moves.insert(vec![field_x], Span::new(30, 40));

        state.clear();

        assert!(state.full_move.is_none());
        assert!(state.partial_moves.is_empty());
        assert!(state.is_empty());
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

        let cv2 = ConstValue::Type(Type::Bool);
        assert_eq!(cv2.as_type(), Some(Type::Bool));
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
        assert_eq!(ConstValue::Bool(true).get_type(), Type::Bool);
        assert_eq!(ConstValue::Type(Type::I32).get_type(), Type::ComptimeType);
        assert_eq!(ConstValue::Unit.get_type(), Type::Unit);
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
