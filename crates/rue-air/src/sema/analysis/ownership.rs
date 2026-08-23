//! Place construction, storage lifetime, move/borrow checks, and assignment analysis.
//!
//! This category is the single owner of semantic places and the state changes
//! caused by reading, writing, moving, copying, borrowing, and reinitializing
//! them. It extends the canonical body-analysis engine rather than
//! introducing peer analysis state.

use super::super::context::{FieldPath, LocalVar, ParamInfo, VariableMoveState};
use super::super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use super::*;
use crate::inst::AirPlaceRef;
use crate::scope::ScopedContext;
use ahash::AHashMap;

/// The position into which a value is being placed when the ADR-0043 two-types
/// string model (RUE-386) requires a *first-class* `str`: a bare `str`
/// parameter, a `str` binding, a `str` return, or a `str` struct field. Only a
/// string literal or another first-class `str` may land in one of these; a
/// string *buffer* (`StrBuf`/`Str(N)`) or a borrowed `str` *view* is rejected
/// because it would escape its backing storage. The variant tailors the
/// diagnostic (only the parameter case gets the `borrow str` did-you-mean).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FirstClassStrSite {
    Param,
    Binding,
    Return,
    Field,
}

impl FirstClassStrSite {
    /// Human-readable position phrase for the diagnostic message.
    fn describe(self) -> &'static str {
        match self {
            FirstClassStrSite::Param => "as a parameter argument",
            FirstClassStrSite::Binding => "in a binding",
            FirstClassStrSite::Return => "as a return value",
            FirstClassStrSite::Field => "in a struct field",
        }
    }
}

/// The escape shape a `-> borrow T` accessor result was caught in
/// (ADR-0062): the result is a second-class borrowed place scoped to its
/// enclosing full expression, so returning, storing, `let`-binding, or
/// capturing it in an aggregate is rejected with a dedicated diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessorEscapeSite {
    Return,
    Store,
    Let,
    Capture,
}

/// The mechanism that gave a `borrow` operand which is not an existing place
/// something to loan (spec 6.1:39–6.1:41, RUE-953).
///
/// The two paths are deliberately distinct and separately criterioned; the
/// choice between them is made once, in
/// [`OrdinaryBodyEngine::elaborate_borrow_operand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorrowOperandElaboration {
    /// **Static promotion.** The operand is comptime-evaluable and infallible,
    /// so the loan names the value's immortal static image: no destructor runs
    /// and no cleanup is scheduled for it (formal core §6.13.2's `H0` story).
    Promoted,
    /// **Compiler-materialized temporary.** The operand is evaluated exactly
    /// once into a fresh hidden binding scoped to the call — the exact extent
    /// of the loan it backs — whose value is dropped exactly once on every
    /// path that leaves that scope (formal core §5.6's scope-exit drop).
    Temporary,
}

/// The AIR pieces of an elaborated `borrow` operand, kept separate so each
/// lands in the position its obligation requires.
pub(crate) struct ElaboratedBorrowOperand {
    /// Addressable read of the hidden binding — the operand's replacement.
    pub(crate) load: AirRef,
    /// Initializer of the hidden binding. Stays at the operand's argument
    /// position, so the operand is evaluated in source order (spec 4.10:7).
    pub(crate) alloc: AirRef,
    /// Storage annotation. Hoisted into a block wrapping the *call*, which is
    /// the scope whose exit drops the binding — after the callee has read
    /// through the loan, on every path that leaves the call.
    pub(crate) storage_live: AirRef,
}

/// Analyzed call operands plus the storage annotations that must wrap the
/// emitted call.
///
/// A borrow-operand temporary's `StorageLive` is hoisted here so the call
/// emitter can wrap the *call* in the block whose exit drops it — after the
/// callee has read through the loan. Its `Alloc` stays at the operand's
/// argument position, so operands are still evaluated left to right
/// (spec 4.10:7).
pub(crate) struct CallOperands {
    pub(crate) args: Vec<AirCallArg>,
    /// `StorageLive` instructions to prefix onto the call, in operand order.
    pub(crate) temp_scope: Vec<AirRef>,
    pub(crate) continues: bool,
}

// Place Building
// ============================================================================

/// Projection info collected during place tracing.
///
/// This extends `AirProjection` with additional metadata needed for type checking
/// and move analysis.
#[derive(Debug)]
struct ProjectionInfo {
    /// The projection to emit
    proj: AirProjection,
    /// The type resulting from this projection
    result_type: Type,
    /// For field projections: the field name (for move checking)
    /// For index projections: None
    field_name: Option<Spur>,
    /// For index projections whose index is a compile-time constant: its
    /// value (for per-element move tracking, RUE-186). None for field
    /// projections and dynamic indices.
    const_index: Option<i64>,
    /// For index projections with a non-negative constant index: the interned
    /// decimal-string path segment for that element (`arr[0]` → `"0"`),
    /// matching the encoding [`super::index_path_segment`] uses for
    /// whole-element moves. Lets `field_path` nest a projection through a
    /// constant index under the correct element (`arr[0].s` → `["0", "s"]`)
    /// instead of stripping the index (RUE-279). None for field projections,
    /// dynamic indices, and negative constants.
    index_segment: Option<Spur>,
}

/// Result of tracing a place expression in RIR.
///
/// This contains all the information needed to build an `AirPlace` and emit
/// a `PlaceRead` or `PlaceWrite` instruction.
#[derive(Debug)]
pub(crate) struct PlaceTrace {
    /// The base of the place (local slot or param slot)
    base: AirPlaceBase,
    /// The type of the base (before projections)
    base_type: Type,
    /// Projections collected during tracing (in order from base to leaf)
    projections: Vec<ProjectionInfo>,
    /// The root variable name (for move checking)
    root_var: Spur,
    /// Whether the root is mutable (for write validation)
    is_root_mutable: bool,
    /// Whether this is a borrow parameter (for error messages)
    is_borrow_param: bool,
    /// Guard statements an inlined accessor call contributed while this
    /// place was traced (ADR-0062). They must execute before the place is
    /// read: every consumer that turns the trace into a value wraps these
    /// around it as a block prefix (see `finish_traced_value`).
    pending_stmts: Vec<AirRef>,
    /// Whether the trace passed through a place-returning accessor call. A
    /// shared result may be read/projected but not written; an exclusive
    /// result may also be written after the receiver and loan checks. Neither
    /// result may be moved out of when it would create an owner alias.
    via_accessor: bool,
    /// Whether strict projection operands reached their result normally.
    continues: bool,
}

#[derive(Clone)]
pub(super) struct MoveStateSnapshot {
    root: Option<Spur>,
    state: Option<VariableMoveState>,
}

impl PlaceTrace {
    /// Get the final type of the place (after all projections).
    pub(crate) fn result_type(&self) -> Type {
        self.projections
            .last()
            .map(|p| p.result_type)
            .unwrap_or(self.base_type)
    }

    /// Build the field path for move checking (list of path segments).
    ///
    /// Fields contribute their name; an index through a **non-negative
    /// constant** contributes its decimal-string element segment, so a
    /// projection through a constant index nests under the right element
    /// (`arr[0].s` → `["0", "s"]`, matching whole-element moves — RUE-279).
    ///
    /// A **dynamic** (or negative) index cannot be named, so it resets the
    /// path: segments before it are dropped and collection continues after it.
    /// A runtime index cannot identify one statically tracked element, so the
    /// caller uses the conservative whole-array E0904 rejection. Constant
    /// indices remain distinct, preserving nested partial moves without
    /// rejecting moves from sibling elements.
    fn field_path(&self) -> Vec<Spur> {
        let mut path = Vec::new();
        for p in &self.projections {
            match p.proj {
                AirProjection::Index { .. } => match p.index_segment {
                    Some(seg) => path.push(seg),
                    // Dynamic/negative index: unnameable element — restart.
                    None => path.clear(),
                },
                AirProjection::Field { .. } => {
                    if let Some(name) = p.field_name {
                        path.push(name);
                    }
                }
            }
        }
        path
    }

    /// The move path of the sub-place reached after the first `depth`
    /// projections — the prefix of [`Self::field_path`] that names the
    /// container `projections[depth]` projects out of.
    ///
    /// Unlike `field_path`, an unnameable segment (a dynamic/negative index,
    /// or a field projection with no recorded name) makes the whole prefix
    /// unnameable (`None`) instead of restarting the path: callers use this
    /// to name a place they are about to record a move for, and a restarted
    /// path would name a DIFFERENT place (RUE-1632).
    fn prefix_field_path(&self, depth: usize) -> Option<Vec<Spur>> {
        let mut path = Vec::with_capacity(depth);
        for p in &self.projections[..depth] {
            match p.proj {
                AirProjection::Index { .. } => path.push(p.index_segment?),
                AirProjection::Field { .. } => path.push(p.field_name?),
            }
        }
        Some(path)
    }

    /// Whether this place contains an index projection whose element cannot be
    /// named statically in the move/drop path model.
    fn has_untrackable_index(&self) -> bool {
        self.projections
            .iter()
            .any(|p| matches!(p.proj, AirProjection::Index { .. }) && p.index_segment.is_none())
    }
}
impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Read move state without exposing the move-state store to phase peers.
    pub(crate) fn moved_state<'ctx>(
        &self,
        ctx: &'ctx AnalysisContext,
        root: &Spur,
    ) -> Option<&'ctx VariableMoveState> {
        ctx.moved_vars.get(root)
    }

    /// Capture one root's move state for a scoped borrow operation.
    pub(super) fn snapshot_move_state(
        &self,
        root: Option<Spur>,
        ctx: &AnalysisContext,
    ) -> MoveStateSnapshot {
        MoveStateSnapshot {
            root,
            state: root.and_then(|root| ctx.moved_vars.get(&root).cloned()),
        }
    }

    /// Capture the legacy value-form snapshot required by string index helpers.
    pub(super) fn snapshot_move_state_value(
        &self,
        root: Option<Spur>,
        ctx: &AnalysisContext,
    ) -> Option<VariableMoveState> {
        root.and_then(|root| ctx.moved_vars.get(&root).cloned())
    }

    /// Restore one root exactly to its state before a scoped borrow.
    pub(super) fn restore_move_state(
        &self,
        snapshot: MoveStateSnapshot,
        ctx: &mut AnalysisContext,
    ) {
        if let Some(root) = snapshot.root {
            match snapshot.state {
                Some(state) => {
                    ctx.moved_vars.insert(root, state);
                }
                None => {
                    ctx.moved_vars.remove(&root);
                }
            }
        }
    }

    /// Restore scoped move state and discard any defensive move marker.
    pub(super) fn restore_move_state_and_cancel(
        &self,
        air: &mut Air,
        value: AirRef,
        snapshot: MoveStateSnapshot,
        ctx: &mut AnalysisContext,
    ) {
        self.restore_move_state(snapshot, ctx);
        air.cancel_move_marker(value);
    }

    /// Discard a move marker when a later operation establishes a borrow.
    pub(super) fn cancel_recorded_move(&self, air: &mut Air, value: AirRef) {
        air.cancel_move_marker(value);
    }

    /// Analyze a value while treating its root as borrowed, restoring context.
    pub(super) fn analyze_with_borrow_root(
        &mut self,
        air: &mut Air,
        value: InstRef,
        root: Option<Spur>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let previous = std::mem::replace(&mut ctx.byref_arg_root, root);
        let result = self.analyze_inst(air, value, ctx);
        ctx.byref_arg_root = previous;
        result
    }

    /// Whether an AIR value names addressable local, parameter, or projected storage.
    pub(super) fn is_addressable_read(&self, air: &Air, value: AirRef) -> bool {
        let mut value = value;
        while let AirInstData::MarkMoved { value: inner, .. } = air.get(value).data {
            value = inner;
        }
        matches!(
            air.get(value).data,
            AirInstData::Load { .. } | AirInstData::Param { .. } | AirInstData::PlaceRead { .. }
        )
    }

    /// Require the analyzed AIR shape promised by an inout or borrow operand.
    ///
    /// RIR place tracing is syntactic and can accept a named constant. This
    /// AIR-boundary check rejects constants and other computed values before
    /// they reach CFG/codegen as by-reference arguments (RUE-760).
    pub(super) fn require_addressable_read(
        &self,
        air: &Air,
        value: AirRef,
        is_inout: bool,
        span: Span,
    ) -> CompileResult<()> {
        if self.is_addressable_read(air, value) {
            Ok(())
        } else {
            Err(CompileError::new(
                if is_inout {
                    ErrorKind::InoutNonLvalue
                } else {
                    ErrorKind::BorrowNonLvalue
                },
                span,
            ))
        }
    }

    /// Allocate and initialize one local storage range under the ownership authority.
    pub(in crate::sema) fn allocate_local_storage(
        &mut self,
        air: &mut Air,
        init: AirRef,
        ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<(u32, AirRef, AirRef)> {
        let slots = self.require_layout_slots(ty, span)?;
        let slot = self.reserve_frame_slots(&mut ctx.next_slot, slots, span)?;
        let live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty,
            span,
        });
        let alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc { slot, init },
            ty: Type::UNIT,
            span,
        });
        Ok((slot, live, alloc))
    }

    /// Trace and read an addressable RIR place through the canonical place owner.
    pub(super) fn try_read_traced_place(
        &mut self,
        air: &mut Air,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<AnalysisResult>> {
        let Some(mut trace) = self.try_trace_place(value, air, ctx)? else {
            return Ok(None);
        };
        let ty = trace.result_type();
        let place = Self::build_place_ref(air, &trace)?;
        let value = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place },
            ty,
            span,
        });
        let value = Self::finish_traced_value(air, &mut trace, value, ty, span)?;
        Ok(Some(AnalysisResult::new(value, ty)))
    }

    /// Whether a by-reference operand names an existing place: a local, a
    /// parameter, or a field/index projection chain rooted at one.
    ///
    /// A name that resolves to no binding — a named constant, or a value
    /// `const` re-export — is *not* a place: it has no caller-visible storage
    /// to point at, which is why it used to be an E0427 (RUE-760) and is now a
    /// borrow-operand elaboration candidate instead (RUE-953).
    fn borrow_operand_names_place(&self, value: InstRef, ctx: &AnalysisContext) -> bool {
        let Some(root) = root_variable_of(self.body_rir_ref(), value) else {
            // A `-> borrow T` accessor chain (ADR-0062) is a place too: it
            // re-roots at its receiver's binding, which the inlined accessor
            // body reads in place.
            return self.place_root_with_accessors(value, ctx).is_some();
        };
        ctx.locals.contains_key(&root) || ctx.has_param(root)
    }

    /// Whether a `borrow` operand qualifies for **static promotion**
    /// (spec 6.1:40, RUE-953).
    ///
    /// The criterion is two conjuncts, both required:
    ///
    /// 1. the operand is drawn from the enumerated *infallible* form set
    ///    below — literals, a named value constant, a `comptime` parameter,
    ///    and the arithmetic, bitwise, comparison, and logical operators over
    ///    them; and
    /// 2. it actually folds to a value under the body's comptime environment
    ///    (a string literal or string constant is its own folded image; the
    ///    comptime engine deliberately holds no string values).
    ///
    /// The form set is *value-independent* so the criterion can be checked by
    /// inspection. `/` and `%` are excluded outright even for a nonzero
    /// literal divisor: their trap conditions (divisor zero, `MIN / -1`) are
    /// value-dependent, and a value-dependent promotion rule is exactly the
    /// mistake Rust had to walk back (RFC 1414 → RFC 3027). Anything outside
    /// the set — a call, an index, a field read, a cast, an intrinsic — takes
    /// the temporary path instead, which is observationally identical apart
    /// from where the value lives.
    ///
    /// Conjunct 2 also rules out a *fallible* fold: an operand whose constant
    /// evaluation overflows or is otherwise diagnosed yields no value here, so
    /// it takes the temporary path and its error is reported by the ordinary
    /// runtime analysis of that temporary.
    fn borrow_operand_is_promotable(&mut self, operand: InstRef, ctx: &AnalysisContext) -> bool {
        if !self.borrow_operand_has_infallible_form(operand, ctx) {
            return false;
        }
        // A string literal (or a `const` bound to one) is `.rodata`-backed and
        // never evaluated: it is already the static image the loan names, and
        // the comptime engine intentionally carries no string values
        // (RUE-957), so asking it to fold would reject the very case the
        // ruling is about.
        if self.borrow_operand_is_static_string(operand, ctx) {
            return true;
        }
        self.try_evaluate_const_in_fn(operand, ctx)
            .is_some_and(|value| {
                matches!(
                    value,
                    ConstValue::Integer(_) | ConstValue::Bool(_) | ConstValue::Unit
                )
            })
    }

    /// Conjunct 1 of the promotion criterion: the operand's syntax is drawn
    /// from the enumerated infallible form set.
    fn borrow_operand_has_infallible_form(&self, operand: InstRef, ctx: &AnalysisContext) -> bool {
        match &self.body_rir_ref().get(operand).data {
            InstData::IntConst(_)
            | InstData::BoolConst(_)
            | InstData::UnitConst
            | InstData::StringConst { .. } => true,
            InstData::Neg { operand }
            | InstData::Not { operand }
            | InstData::BitNot { operand } => {
                self.borrow_operand_has_infallible_form(*operand, ctx)
            }
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs }
            | InstData::And { lhs, rhs }
            | InstData::Or { lhs, rhs }
            | InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => {
                self.borrow_operand_has_infallible_form(*lhs, ctx)
                    && self.borrow_operand_has_infallible_form(*rhs, ctx)
            }
            // A name is in the set only when it is compile-time known and is
            // NOT a runtime binding: a `let` or an ordinary parameter is a
            // place and never reaches elaboration, while a named value
            // constant or a `comptime` parameter has a static image.
            InstData::VarRef { name, .. } => {
                !ctx.locals.contains_key(name)
                    && !ctx.has_param(*name)
                    && (self.borrow_operand_is_static_string(operand, ctx)
                        || ctx.comptime_value_vars.contains_key(name)
                        || self
                            .call_facts()
                            .call_value_const(self.body_rir_ref().get(operand).span.file_id, *name)
                            .is_some())
            }
            _ => false,
        }
    }

    /// Whether the operand is a `.rodata`-backed string image: an inline
    /// string literal, or a name bound to a string `const` (RUE-957).
    fn borrow_operand_is_static_string(&self, operand: InstRef, ctx: &AnalysisContext) -> bool {
        match &self.body_rir_ref().get(operand).data {
            InstData::StringConst { .. } => true,
            InstData::VarRef { name, .. } => {
                !ctx.locals.contains_key(name)
                    && !ctx.has_param(*name)
                    && self
                        .call_facts()
                        .call_value_const(self.body_rir_ref().get(operand).span.file_id, *name)
                        .is_some_and(|info| matches!(info.value, ConstValue::String(_)))
            }
            _ => false,
        }
    }

    /// Elaborate a `borrow` operand that denotes no existing place into one
    /// that does (spec 6.1:39–6.1:41, RUE-953).
    ///
    /// Returns the hidden binding's three AIR pieces separately, because each
    /// belongs in a different position. The split is what makes both
    /// obligations hold at once:
    ///
    /// - the `Alloc` stays here, at the operand's argument position, so the
    ///   operand is evaluated in source order (spec 4.10:7);
    /// - the `StorageLive` opens its drop scope around the *call*, so a
    ///   temporary is dropped after the callee has read through the loan, by
    ///   the ordinary scope-exit machinery — which already covers early exits
    ///   (`return`, `break`, `?`) and path-dependent moves through its drop
    ///   flags. No new drop path is introduced.
    ///
    /// A promoted operand's slot is registered as non-owning, so drop
    /// elaboration schedules nothing for it at all.
    fn elaborate_borrow_operand(
        &mut self,
        air: &mut Air,
        operand: InstRef,
        value: AirRef,
        ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<ElaboratedBorrowOperand> {
        // The hidden binding is never named, so nothing can consume it: its
        // value reaches the end of the statement owned and is dropped. That is
        // precisely the condition the linear leak check forbids, so a linear
        // operand is reported through the language's existing
        // discarded-linear-value diagnostic — with the help text that already
        // names the fix ("bind the value with `let` and consume it").
        self.reject_discarded_linear_value(ty, operand)?;
        let kind = if self.borrow_operand_is_promotable(operand, ctx) {
            BorrowOperandElaboration::Promoted
        } else {
            BorrowOperandElaboration::Temporary
        };
        let slots = self.require_layout_slots(ty, span)?;
        let slot = self.reserve_frame_slots(&mut ctx.next_slot, slots, span)?;
        if kind == BorrowOperandElaboration::Promoted {
            // The promoted image is immortal and owns nothing — a literal's
            // bytes live in `.rodata`, and a `StrBuf` promoted at a `borrow
            // StrBuf` position is the non-owning `cap == 0` header over them.
            // Registering the slot as non-owning is what makes "no drop glue"
            // structural rather than incidental.
            air.add_borrow_slot(slot);
        }
        let live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty,
            span,
        });
        let alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc { slot, init: value },
            ty: Type::UNIT,
            span,
        });
        let load = air.add_inst(AirInst {
            data: AirInstData::Load { slot },
            ty,
            span,
        });
        Ok(ElaboratedBorrowOperand {
            load,
            alloc,
            storage_live: live,
        })
    }

    /// Give a compiler-synthesized borrow argument an addressable home.
    pub(crate) fn materialize_borrow_argument(
        &mut self,
        air: &mut Air,
        value: AirRef,
        ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<(AirRef, Vec<AirRef>)> {
        let (value, mut prefix) = self.peel_projected_rvalue_scope(air, value);
        if self.is_addressable_read(air, value) {
            return Ok((value, prefix));
        }
        let slots = self.require_layout_slots(ty, span)?;
        let slot = self.reserve_frame_slots(&mut ctx.next_slot, slots, span)?;
        let live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty,
            span,
        });
        let alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc { slot, init: value },
            ty: Type::UNIT,
            span,
        });
        let load = air.add_inst(AirInst {
            data: AirInstData::Load { slot },
            ty,
            span,
        });
        prefix.extend([live, alloc]);
        Ok((load, prefix))
    }

    pub(crate) fn wrap_value_with_temp_scope(
        &mut self,
        air: &mut Air,
        value: AirRef,
        ty: Type,
        span: Span,
        prefix: Vec<AirRef>,
    ) -> CompileResult<AirRef> {
        if prefix.is_empty() {
            return Ok(value);
        }
        Ok(air.add_block(&prefix, value, ty, span)?)
    }

    /// Read the shared `{ptr, len}` prefix from one StrBuf place owner.
    pub(crate) fn project_strbuf_text_fields(
        &mut self,
        air: &mut Air,
        value: AirRef,
        ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<(AirRef, AirRef, Vec<AirRef>)> {
        let struct_id = ty.as_struct().expect("StrBuf is a struct");
        // Read the `{ptr, len}` prefix through the trusted source accessors
        // (`as_ptr()`, `len()`) rather than by projecting fields 0 and 1, so the
        // compiler never depends on StrBuf's internal field layout (RUE-1066).
        // Both accessors are `borrow self`, so one materialized borrow of the
        // owner backs both calls.
        let ptr_method = self.body_interner().get_or_intern("as_ptr");
        let len_method = self.body_interner().get_or_intern("len");
        let Some(ptr_ty) = self
            .call_facts()
            .call_method_info(struct_id, ptr_method)
            .map(|m| m.return_type)
        else {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "trusted std StrBuf is missing its source `as_ptr` method".to_string(),
                ),
                span,
            ));
        };
        let Some(len_ty) = self
            .call_facts()
            .call_method_info(struct_id, len_method)
            .map(|m| m.return_type)
        else {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "trusted std StrBuf is missing its source `len` method".to_string(),
                ),
                span,
            ));
        };
        ctx.referenced_methods.insert((struct_id, ptr_method));
        ctx.referenced_methods.insert((struct_id, len_method));
        {
            self.record_body_method_dependency((struct_id, ptr_method));
            self.record_body_method_dependency((struct_id, len_method));
        }
        let (receiver, prefix) = self.materialize_borrow_argument(air, value, ty, span, ctx)?;
        let ptr_call_name = self
            .body_interner()
            .get_or_intern(&self.method_symbol(struct_id, "as_ptr", true));
        let len_call_name = self
            .body_interner()
            .get_or_intern(&self.method_symbol(struct_id, "len", true));
        let ptr_ref = air.add_call(
            None,
            ptr_call_name,
            &[AirCallArg {
                value: receiver,
                mode: AirArgMode::Borrow,
            }],
            ptr_ty,
            span,
        )?;
        let len_ref = air.add_call(
            None,
            len_call_name,
            &[AirCallArg {
                value: receiver,
                mode: AirArgMode::Borrow,
            }],
            len_ty,
            span,
        )?;
        Ok((ptr_ref, len_ref, prefix))
    }

    /// Build a by-value, layout-stable `str` view `{ptr, len}` from a StrBuf
    /// source.
    ///
    /// The `{ptr, len}` prefix is read through the trusted `as_ptr()`/`len()`
    /// accessors (see [`Self::project_strbuf_text_fields`]), then packed into
    /// the synthetic `str` struct — whose 2-word `{ptr, len}` layout is fixed
    /// (ADR-0043 Phase 3). Routing `@dbg`/`@panic`/`borrow str` coercion
    /// through this view means no consumer ever depends on StrBuf's own field
    /// layout (RUE-1066): the runtime text helpers see a `str`, and StrBuf's
    /// header can be reordered freely.
    ///
    /// The returned prefix must wrap the view (or a value derived from it) via
    /// [`Self::wrap_value_with_temp_scope`] so any materialized owner stays
    /// live across the borrow — the view is a second-class borrow into the
    /// StrBuf's buffer and must not outlive it.
    pub(crate) fn strbuf_text_view(
        &mut self,
        air: &mut Air,
        value: AirRef,
        ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<(AirRef, Vec<AirRef>)> {
        let (ptr, len, prefix) = self.project_strbuf_text_fields(air, value, ty, span, ctx)?;
        let str_ty = self.get_or_create_str_struct(span)?;
        let str_struct_id = str_ty.as_struct().expect("str is a synthetic struct");
        let view = air.add_struct_init(str_struct_id, &[ptr, len], &[0, 1], str_ty, span)?;
        Ok((view, prefix))
    }

    /// Peel the synthetic scope produced by [`Self::emit_projected_rvalue_read`].
    ///
    /// A projected read from a computed owner must keep that owner alive when
    /// the read is subsequently borrowed by an outer call. Returning the scope
    /// separately lets the consumer wrap the *call*, rather than lowering a
    /// nested block that drops the owner before the call begins.
    pub(super) fn peel_projected_rvalue_scope(
        &self,
        air: &Air,
        value: AirRef,
    ) -> (AirRef, Vec<AirRef>) {
        let original = value;
        let mut value = value;
        while let AirInstData::MarkMoved { value: inner, .. } = air.get(value).data {
            value = inner;
        }
        let AirInstData::Block {
            statements,
            value: projected,
        } = &air.get(value).data
        else {
            return (original, Vec::new());
        };
        if statements.len() < 2 || statements.len() % 2 != 0 {
            return (original, Vec::new());
        }
        let statements = air.get_block_statements(statements).collect::<Vec<_>>();
        let mut projected = *projected;
        while let AirInstData::MarkMoved { value: inner, .. } = air.get(projected).data {
            projected = inner;
        }
        let AirInstData::PlaceRead { place } = air.get(projected).data else {
            return (original, Vec::new());
        };
        let place_base = air.get_place(place).base;
        // An accessor guards block is also a scope around an addressable place.
        // A by-ref consumer must receive the tail PlaceRead itself while the
        // guards stay as prefix statements around that consumer; otherwise CFG
        // lowering turns the block value into a join value and loses its place
        // identity before mandatory splicing.
        if matches!(place_base, AirPlaceBase::Accessor(_)) {
            return (projected, statements);
        }
        let AirPlaceBase::Local(owner_slot) = place_base else {
            return (original, Vec::new());
        };
        let owns_projected_place = statements.windows(2).any(|pair| {
            matches!(
                air.get(pair[0]).data,
                AirInstData::StorageLive { slot } if slot == owner_slot
            ) && matches!(
                air.get(pair[1]).data,
                AirInstData::Alloc { slot, .. } if slot == owner_slot
            )
        });
        if !owns_projected_place {
            return (original, Vec::new());
        }
        (projected, statements)
    }

    /// Materialize an rvalue in a temporary and read one projection from it.
    ///
    /// AIR models every projected memory access as a place operation. Values
    /// that do not already name a local or parameter therefore need a short-
    /// lived addressable home before they can be projected.
    pub(super) fn emit_projected_rvalue_read(
        &mut self,
        air: &mut Air,
        base: AirRef,
        base_type: Type,
        projection: AirProjection,
        result_type: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AirRef> {
        // Nested computed projections must share one enclosing lifetime. If the
        // computed base is itself a projected-rvalue block, extend its place
        // path in the original owner. Materializing the intermediate field in a
        // second owned temporary would duplicate non-Copy bits and drop them
        // twice.
        let (base, mut stmts) = self.peel_projected_rvalue_scope(air, base);
        if !stmts.is_empty()
            && let AirInstData::PlaceRead { place } = air.get(base).data
        {
            let place = air.get_place(place);
            let base = place.base;
            let base_type = place.base_type;
            let mut projections = air.get_place_projections(place).to_vec();
            projections.push(projection);
            let place = air.make_place(base, base_type, projections)?;
            let value = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place },
                ty: result_type,
                span,
            });
            return Ok(air.add_block(&stmts, value, result_type, span)?);
        }
        let slots = self.require_layout_slots(base_type, span)?;
        let temp_slot = self.reserve_frame_slots(&mut ctx.next_slot, slots, span)?;

        let storage_live = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });
        let alloc = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base,
            },
            ty: Type::UNIT,
            span,
        });
        let place = air.make_place(AirPlaceBase::Local(temp_slot), base_type, [projection])?;
        let value = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place },
            ty: result_type,
            span,
        });
        stmts.extend([storage_live, alloc]);
        Ok(air.add_block(&stmts, value, result_type, span)?)
    }

    // ========================================================================
    // Place Tracing
    // ========================================================================

    /// Try to trace an RIR expression to a place (lvalue).
    ///
    /// This walks the RIR instruction chain backward from a `FieldGet` or `IndexGet`
    /// to find the root `VarRef`, collecting projections along the way.
    ///
    /// Returns `None` if the expression is not a place (e.g., a function call result).
    ///
    /// # Arguments
    /// * `inst_ref` - The RIR instruction to trace
    /// * `air` - The AIR being built (needed to analyze index expressions)
    /// * `ctx` - Analysis context with local/param info
    ///
    /// # Returns
    /// * `Some(PlaceTrace)` if the expression is a place
    /// * `None` if it's not (e.g., `get_struct().field` where base is a call)
    pub(crate) fn try_trace_place(
        &mut self,
        inst_ref: InstRef,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        let trace = self.try_trace_place_inner(inst_ref, air, ctx)?;
        if let Some(trace) = &trace {
            // Any place access through a projection (field read `s.a`, array
            // index `a[i]`, method receiver `h.s.len()`, field/index write
            // `s.a = ...`) uses its root variable. Mark it here, at the single
            // shared tracing point, so the unused-variable lint doesn't fire
            // on variables that are only accessed through projections
            // (RUE-135). Direct assignment to the variable itself (`x = 5`)
            // does not go through place tracing and intentionally still
            // counts as unused.
            ctx.used_locals.insert(trace.root_var);
        }
        Ok(trace)
    }

    /// Inner implementation that accumulates projections.
    fn try_trace_place_inner(
        &mut self,
        inst_ref: InstRef,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

        match &inst.data {
            // Base case: local variable reference
            InstData::VarRef { name, .. } => {
                // An accessor-inline place alias (`self` inside an inlined
                // accessor body, ADR-0062) substitutes the caller's receiver
                // place: the trace roots at the caller's own root variable,
                // and the alias is a shared borrow — read-only, never moved
                // out of. Aliases are installed only for the inline scope and
                // take precedence over any same-named caller binding.
                if let Some(alias) = ctx.place_aliases.get(name) {
                    return Ok(Some(PlaceTrace {
                        base: alias.base,
                        base_type: alias.base_type,
                        projections: alias
                            .projections
                            .iter()
                            .map(|p| ProjectionInfo {
                                proj: p.proj,
                                result_type: p.result_type,
                                field_name: p.field_name,
                                const_index: p.const_index,
                                index_segment: p.index_segment,
                            })
                            .collect(),
                        root_var: alias.root_var,
                        is_root_mutable: false,
                        is_borrow_param: true,
                        pending_stmts: Vec::new(),
                        via_accessor: true,
                        continues: true,
                    }));
                }

                // Locals shadow parameters (spec 5.1:10): a `let` that rebinds a
                // parameter name makes every later reference resolve to the new
                // local, not the parameter (RUE-278). A local with a param's
                // name can only arise by shadowing, so locals always win here.
                if let Some(local) = ctx.locals.get(name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Local(local.slot),
                        base_type: local.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        is_root_mutable: local.is_mut,
                        is_borrow_param: false,
                        pending_stmts: Vec::new(),
                        via_accessor: false,
                        continues: true,
                    }));
                }

                // Otherwise it may be a parameter.
                if let Some(param_info) = ctx.param(*name) {
                    return Ok(Some(PlaceTrace {
                        base: AirPlaceBase::Param(param_info.abi_slot),
                        base_type: param_info.ty,
                        projections: Vec::new(),
                        root_var: *name,
                        // `inout` names mutable caller storage; a `mut self`
                        // receiver is a mutable by-value binding (callee copy
                        // only).
                        is_root_mutable: matches!(param_info.mode, RirParamMode::Inout)
                            || param_info.is_mut,
                        is_borrow_param: matches!(param_info.mode, RirParamMode::Borrow),
                        pending_stmts: Vec::new(),
                        via_accessor: false,
                        continues: true,
                    }));
                }

                // Not a variable - might be a constant or type name
                Ok(None)
            }

            // Recursive case: field access
            InstData::FieldGet { base, field } => {
                // First, recursively trace the base
                let base_trace = self.try_trace_place_inner(*base, air, ctx)?;

                match base_trace {
                    Some(mut trace) => {
                        // Get the struct type from the base
                        let base_type = trace.result_type();
                        let struct_id = match base_type.as_struct() {
                            Some(id) => id,
                            None => {
                                // Module access or non-struct - not a place
                                return Ok(None);
                            }
                        };

                        // Look up field info
                        let struct_def = self.body_type_pool().struct_def(struct_id);
                        let field_name_str = self.body_interner().resolve(field);
                        let (field_index, struct_field) =
                            match struct_def.find_field(field_name_str) {
                                Some(info) => info,
                                None => return Ok(None), // Unknown field
                            };

                        let field_type = struct_field.ty;

                        // Add this projection with field name for move checking
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Field {
                                struct_id,
                                field_index: field_index as u32,
                            },
                            result_type: field_type,
                            field_name: Some(*field),
                            const_index: None,
                            index_segment: None,
                        });

                        Ok(Some(trace))
                    }
                    None => {
                        // Base is not a place (e.g., function call result)
                        Ok(None)
                    }
                }
            }

            // Recursive case: array index
            InstData::IndexGet { base, index } => {
                // First, recursively trace the base
                let base_trace = self.try_trace_place_inner(*base, air, ctx)?;

                match base_trace {
                    Some(mut trace) => {
                        // Get the array type from the base
                        let base_type = trace.result_type();
                        let (_array_type_id, elem_type) = match base_type.as_array() {
                            Some(id) => {
                                let (elem, _len) = self.body_type_pool().array_def(id);
                                (id, elem)
                            }
                            None => return Ok(None), // Not an array
                        };

                        // Analyze the index expression to get an AirRef. The
                        // index is an rvalue of its own, not part of the
                        // place: a surrounding by-ref argument's borrow
                        // (`byref_arg_root`, RUE-143) must not leak into it
                        // (in `f(inout a[take(x)])` the call `take(x)` moves
                        // x normally, even if x happens to be the root).
                        let saved_byref_root = ctx.byref_arg_root.take();
                        let index_result = self.analyze_inst(air, *index, ctx);
                        ctx.byref_arg_root = saved_byref_root;
                        let index_result = index_result?;
                        trace.continues &= index_result.continues;

                        // A place trace can be built before the outer read or
                        // write path performs its normal index validation. Do
                        // the same AIR-level check here so generic analysis
                        // cannot record a `type`-valued projection and defer
                        // the failure to whole-body AIR validation.
                        let index_type = air.get(index_result.air_ref).ty;
                        if !index_type.is_integer() {
                            return Err(CompileError::new(
                                ErrorKind::TypeMismatch {
                                    expected: "integer type".to_string(),
                                    found: index_type
                                        .safe_name_with_pool(Some(self.body_type_pool())),
                                },
                                self.body_rir_ref().get(*index).span,
                            ));
                        }

                        // Add this projection (no field name for indices).
                        // A non-negative constant index also gets its interned
                        // element path segment so field_path can nest through
                        // it (RUE-279); negative/dynamic indices leave it None.
                        let const_index = self.try_get_const_index(*index);
                        let index_segment = match const_index {
                            Some(k) if k >= 0 => {
                                Some(super::index_path_segment(self.body_interner(), k as u64))
                            }
                            _ => None,
                        };
                        trace.projections.push(ProjectionInfo {
                            proj: AirProjection::Index {
                                array_type: base_type,
                                index: index_result.air_ref,
                            },
                            result_type: elem_type,
                            field_name: None,
                            const_index,
                            index_segment,
                        });

                        Ok(Some(trace))
                    }
                    None => {
                        // Base is not a place
                        Ok(None)
                    }
                }
            }

            // A `-> borrow T` accessor call composes as a place (ADR-0062):
            // `v.get_ref(i).name` traces through the inlined accessor result.
            // A non-accessor method call is not a place.
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                let args = args.clone();
                self.try_trace_accessor_call(inst_ref, *receiver, *method, &args, air, ctx)
            }

            // Not a place expression
            _ => Ok(None),
        }
    }

    /// Trace a method call as a place when — and only when — it resolves to a
    /// `-> borrow T` accessor (ADR-0062). Returns `Ok(None)` for every other
    /// method call so value analysis handles it.
    fn try_trace_accessor_call(
        &mut self,
        inst_ref: InstRef,
        receiver: InstRef,
        method: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        air: &mut Air,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Option<PlaceTrace>> {
        if let Some((place, root, ty, mutable, continues)) =
            ctx.accessor_place_refs.get(&inst_ref).copied()
        {
            let cached = air.get_place(place);
            return Ok(Some(PlaceTrace {
                base: cached.base,
                base_type: ty,
                projections: Vec::new(),
                root_var: root,
                is_root_mutable: mutable,
                is_borrow_param: !mutable,
                pending_stmts: Vec::new(),
                via_accessor: true,
                continues,
            }));
        }
        // Peek the receiver type without emitting anything: only proceed when
        // the method is a registered accessor.
        let Some(struct_id) = self
            .peek_place_type(receiver, ctx)
            .and_then(|ty| ty.as_struct())
        else {
            return Ok(None);
        };
        let Some(info) = self.call_facts().call_method_info(struct_id, method) else {
            return Ok(None);
        };
        if !info.returns_borrow && !info.returns_inout {
            return Ok(None);
        }
        let span = self.body_rir_ref().get(inst_ref).span;
        self.expand_accessor_call(
            air, inst_ref, receiver, struct_id, method, args_range, span, ctx,
        )
        .map(Some)
    }

    /// Lower a place-returning accessor call to a marked place-producing call.
    ///
    /// Semantic analysis establishes only the call-site loan and escape
    /// contract. The callee body remains an exact CFG-query dependency and is
    /// mandatorily spliced before code generation; no calling convention for
    /// returning a place exists. Statically this is the
    /// (Accessor-Call) rule: the receiver must be a place; the result is a
    /// second-class place whose shared or exclusive loan on the receiver root
    /// spans the enclosing full expression (registered in
    /// `ctx.expression_loans`).
    #[allow(clippy::too_many_arguments)]
    fn expand_accessor_call(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        receiver: InstRef,
        struct_id: StructId,
        method: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<PlaceTrace> {
        let expression_ledgers_before_call = ctx.checkpoint_expression_ledgers();
        let info = self
            .call_facts()
            .call_method_info(struct_id, method)
            .expect("accessor expansion follows a successful method lookup");
        let method_name_str = self.body_interner().resolve(&method).to_string();
        let call_name = self.method_symbol(struct_id, &method_name_str, true);
        let call_name = self.body_interner().get_or_intern(&call_name);
        if self.function_identity(call_name).is_ok_and(|identity| {
            ctx.canonical_function_identity == crate::FunctionInstanceKey::Definition(identity)
        }) {
            return Err(CompileError::new(
                ErrorKind::AccessorRecursion {
                    method: method_name_str.clone(),
                },
                span,
            )
            .with_note(crate::declaration_validation::ACCESSOR_RECURSION_NOTE));
        }

        // RUE-1208: semantic analysis establishes the accessor's place/loan
        // contract, but does not inspect or expand the callee body. The
        // marked place-producing call survives through canonical AIR so the
        // per-function CFG query can depend on and splice the callee CFG.
        let receiver_span = self.body_rir_ref().get(receiver).span;
        let mut receiver_trace = {
            let prev_byref_root = ctx.byref_arg_root.take();
            let trace = self.try_trace_place(receiver, air, ctx);
            ctx.byref_arg_root = prev_byref_root;
            trace?.ok_or_else(|| CompileError::new(ErrorKind::BorrowNonLvalue, receiver_span))?
        };
        let root = receiver_trace.root_var;
        let accessor_loan_kind = if info.returns_inout {
            CallLoanKind::Inout
        } else {
            CallLoanKind::Borrow
        };
        // The innermost frame is the argument list currently being lowered.
        // Its own `inout` entry is the accessor's enclosing exclusive access,
        // not a second loan; outer frames still represent genuinely nested
        // calls and must conflict.
        let frame_count = ctx.call_loaned_roots.len();
        for (frame_index, frame) in ctx.call_loaned_roots.iter().enumerate() {
            if frame.iter().any(|(r, kind)| {
                *r == root
                    && !(frame_index + 1 == frame_count && *kind == CallLoanKind::Inout)
                    && (*kind == CallLoanKind::Inout || accessor_loan_kind == CallLoanKind::Inout)
            }) {
                return Err(CompileError::new(
                    ErrorKind::AccessorLoanConflict {
                        variable: self.body_interner().resolve(&root).to_string(),
                        conflict: "as an accessor receiver",
                    },
                    span,
                ));
            }
        }
        let receiver_ty = receiver_trace.result_type();
        if receiver_ty.as_struct() != Some(struct_id) {
            return Err(self.type_mismatch_error(Type::new_struct(struct_id), receiver_ty, span));
        }
        if info.returns_inout && !receiver_trace.is_root_mutable {
            let root_name = self.body_interner().resolve(&root).to_string();
            return Err(CompileError::new(
                if receiver_trace.is_borrow_param {
                    ErrorKind::MutateBorrowedValue {
                        variable: root_name,
                    }
                } else {
                    ErrorKind::AssignToImmutable(root_name)
                },
                span,
            ));
        }
        // Analyze guard operands before installing the result loan. Under the
        // accessor-call evaluation rule, no accessor result exists when the
        // receiver or a guard operand diverges.
        let loan_kind = if info.returns_inout {
            CallLoanKind::Inout
        } else {
            CallLoanKind::Borrow
        };
        let param_data = self.body_param_data(info.params);
        let param_types = param_data.types().to_vec();
        let param_modes = param_data.modes().to_vec();
        self.validate_call_contract_for_accessor(args_range, &param_types, &param_modes, span)?;
        let operands = self.analyze_call_operands(
            air,
            args_range,
            &param_types,
            &param_modes,
            false,
            true,
            ctx,
        )?;
        let accessor_continues = receiver_trace.continues && operands.continues;
        if !accessor_continues {
            ctx.rollback_expression_ledgers(expression_ledgers_before_call);
        }
        if accessor_continues {
            // Mutable accessors carry an exclusive loan; shared and exclusive
            // accessor results conflict on the same root. The root loan list
            // is intentionally expression-scoped and root-granular.
            if ctx.expression_loans.iter().any(|(r, _, kind)| {
                *r == root && (*kind == CallLoanKind::Inout || loan_kind == CallLoanKind::Inout)
            }) {
                return Err(CompileError::new(
                    ErrorKind::AccessorLoanConflict {
                        variable: self.body_interner().resolve(&root).to_string(),
                        conflict: "for another accessor result",
                    },
                    span,
                ));
            }
            if let Some((_, exclusive_span)) = ctx
                .expression_exclusive_uses
                .iter()
                .find(|(used_root, _)| *used_root == root)
            {
                return Err(CompileError::new(
                    ErrorKind::AccessorLoanConflict {
                        variable: self.body_interner().resolve(&root).to_string(),
                        conflict: "for an accessor result after an exclusive use",
                    },
                    span,
                )
                .with_label("exclusive use here", *exclusive_span));
            }
            if loan_kind == CallLoanKind::Inout
                && ctx
                    .expression_shared_reads
                    .iter()
                    .any(|(read_root, _)| *read_root == root)
            {
                return Err(CompileError::new(
                    ErrorKind::AccessorLoanConflict {
                        variable: self.body_interner().resolve(&root).to_string(),
                        conflict: "for an exclusive accessor result after a shared read",
                    },
                    span,
                ));
            }
            ctx.expression_loans.push((root, span, loan_kind));
        }
        ctx.accessor_call_insts.insert(inst_ref, (method, root));
        ctx.referenced_methods.insert((struct_id, method));
        self.record_body_method_dependency((struct_id, method));
        let receiver_place = Self::build_place_ref(air, &receiver_trace)?;
        let receiver_value = air.add_inst(AirInst {
            data: AirInstData::PlaceRead {
                place: receiver_place,
            },
            ty: receiver_ty,
            span: receiver_span,
        });
        let mut call_args = Vec::with_capacity(operands.args.len() + 1);
        call_args.push(AirCallArg {
            value: receiver_value,
            mode: if info.returns_inout {
                AirArgMode::Inout
            } else {
                AirArgMode::Borrow
            },
        });
        call_args.extend(operands.args);
        let call = air.add_accessor_call(call_name, &call_args, info.return_type, span)?;
        receiver_trace.base = AirPlaceBase::Accessor(call);
        receiver_trace.base_type = info.return_type;
        receiver_trace.projections.clear();
        receiver_trace.pending_stmts.extend(operands.temp_scope);
        receiver_trace.via_accessor = true;
        receiver_trace.is_borrow_param = !info.returns_inout;
        receiver_trace.is_root_mutable = info.returns_inout;
        receiver_trace.continues = accessor_continues;
        let yielded_place = Self::build_place_ref(air, &receiver_trace)?;
        ctx.accessor_place_refs.insert(
            inst_ref,
            (
                yielded_place,
                root,
                info.return_type,
                info.returns_inout,
                accessor_continues,
            ),
        );
        Ok(receiver_trace)
    }

    /// Analyze a place-returning accessor call in value position (ADR-0062):
    /// expand it to guards plus the yielded place, then read the place. The
    /// result remains a borrowed place, so a drop-glue element may only flow to
    /// a by-ref consumer (a `borrow` argument or by-ref receiver, which take
    /// the place's address); reading it out by value would mint an aliasing
    /// owner (E0258) — exactly the RUE-651 double-free the E0711 gate closes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn analyze_accessor_call_value(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        receiver: InstRef,
        struct_id: StructId,
        method: Spur,
        args_range: &rue_rir::RirCallArgsRange,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let mut trace = self.expand_accessor_call(
            air, inst_ref, receiver, struct_id, method, args_range, span, ctx,
        )?;
        let ty = trace.result_type();
        let byref_consumer = ctx
            .byref_arg_root
            .is_some_and(|root| root == trace.root_var);
        if !byref_consumer && self.type_has_drop_glue(ty) {
            return Err(CompileError::new(
                ErrorKind::AccessorResultMoved {
                    ty: ty.safe_name_with_pool(Some(self.body_type_pool())),
                },
                span,
            )
            .with_help(
                "project a field, call a `borrow self` method, compare, or pass the \
                 result as a `borrow` argument instead of copying it out",
            ));
        }
        let place = Self::build_place_ref(air, &trace)?;
        let value = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place },
            ty,
            span,
        });
        let value = Self::finish_traced_value(air, &mut trace, value, ty, span)?;
        Ok(AnalysisResult::with_continues(value, ty, trace.continues))
    }

    /// The syntactic root of a place expression that may pass through
    /// `-> borrow T` accessor calls (ADR-0062): `v.get_ref(i).name` roots at
    /// `v`. Extends `root_variable_of` with accessor-call links; `None` when
    /// the expression is not a place chain.
    pub(crate) fn place_root_with_accessors(
        &self,
        inst_ref: InstRef,
        ctx: &AnalysisContext,
    ) -> Option<Spur> {
        match &self.body_rir_ref().get(inst_ref).data {
            InstData::VarRef { name, .. } => Some(*name),
            InstData::FieldGet { base, .. } | InstData::IndexGet { base, .. } => {
                self.place_root_with_accessors(*base, ctx)
            }
            InstData::MethodCall {
                receiver, method, ..
            } => {
                let base_ty = self.peek_place_type(*receiver, ctx)?;
                let base_struct = base_ty.as_struct()?;
                let info = self.call_facts().call_method_info(base_struct, *method)?;
                if !info.returns_borrow && !info.returns_inout {
                    return None;
                }
                self.place_root_with_accessors(*receiver, ctx)
            }
            _ => None,
        }
    }

    /// The accessor variant of `validate_call_contract`: checks arity and
    /// explicit argument modes without the by-ref exclusivity pass (accessor
    /// parameters are all by-value).
    fn validate_call_contract_for_accessor(
        &self,
        args_range: &rue_rir::RirCallArgsRange,
        param_types: &[Type],
        param_modes: &[RirParamMode],
        span: Span,
    ) -> CompileResult<()> {
        let args = self.body_rir_ref().call_args(args_range).to_vec();
        if args.len() != param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }
        self.validate_explicit_call_modes(&args, param_modes.iter().copied())
    }

    /// Turn a traced place into its readable AIR value, prefixing any pending
    /// accessor guard statements so they execute before the read.
    pub(super) fn finish_traced_value(
        air: &mut Air,
        trace: &mut PlaceTrace,
        value: AirRef,
        ty: Type,
        span: Span,
    ) -> CompileResult<AirRef> {
        let pending = std::mem::take(&mut trace.pending_stmts);
        if pending.is_empty() {
            Ok(value)
        } else {
            Ok(air.add_block(&pending, value, ty, span)?)
        }
    }

    /// Build an AirPlaceRef from a PlaceTrace, adding projections to the Air.
    pub(crate) fn build_place_ref(air: &mut Air, trace: &PlaceTrace) -> CompileResult<AirPlaceRef> {
        let projs = trace.projections.iter().map(|p| p.proj);
        Ok(air.make_place(trace.base, trace.base_type, projs)?)
    }

    /// Build a `MarkMoved` place from a trace.
    ///
    /// Normal place reads keep the user's index expression, but move markers
    /// must be statically decodable by drop elaboration. Re-emit constant index
    /// projections with dedicated `Const` instructions so CFG lowering can turn
    /// paths like `arr[0].field` into the moved path `[0, field]`.
    ///
    /// `projections` is the prefix of the trace's projections that names the
    /// moved place. It is the whole chain for an ordinary field move, and a
    /// strict prefix when the consumed place is an ancestor of the accessed
    /// one — a nested declared-`linear` destructure consumes the struct it
    /// destructures, not the leaf it extracts (RUE-1632).
    fn build_move_marker_place_ref(
        air: &mut Air,
        trace: &PlaceTrace,
        projections: &[ProjectionInfo],
        span: Span,
    ) -> CompileResult<AirPlaceRef> {
        let mut projs = Vec::with_capacity(projections.len());
        for p in projections {
            match p.proj {
                AirProjection::Field { .. } => projs.push(p.proj),
                AirProjection::Index { array_type, .. } => {
                    let k = p.const_index.ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::InternalError(
                                "move-marker index is not statically trackable".to_string(),
                            ),
                            span,
                        )
                    })?;
                    debug_assert!(k >= 0, "move-marker index must be non-negative");
                    let index = air.add_inst(AirInst {
                        data: AirInstData::Const(k as u64),
                        ty: Type::U64,
                        span,
                    });
                    projs.push(AirProjection::Index { array_type, index });
                }
            }
        }
        Ok(air.make_place(trace.base, trace.base_type, projs)?)
    }
    // ========================================================================
    // Variable operations: Alloc, VarRef, Assign
    // ========================================================================

    /// Analyze a variable operation instruction.
    ///
    /// Handles: Alloc, VarRef, Assign
    pub(super) fn analyze_variable_ops(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = {
            let source = self.body_rir_ref().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };

        match &inst.data {
            InstData::Alloc {
                directives,
                name,
                is_mut,
                ty,
                init,
                iter_elem,
            } => self.analyze_alloc(
                air, directives, *name, *is_mut, *ty, *init, *iter_elem, inst.span, ctx,
            ),

            InstData::VarRef { name, anchor } => {
                let resolved_ty = ctx.resolved_type_of(inst_ref);
                self.analyze_var_ref(air, *name, anchor.clone(), inst.span, resolved_ty, ctx)
            }

            InstData::Assign { name, value } => {
                self.analyze_assign(air, *name, *value, inst.span, ctx)
            }

            _ => Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "analyze_variable_ops called with non-variable instruction: {:?}",
                    inst.data
                )),
                inst.span,
            )),
        }
    }

    /// Analyze a local variable allocation.
    fn analyze_alloc(
        &mut self,
        air: &mut Air,
        directives: &rue_rir::RirDirectivesRange,
        name: Option<Spur>,
        is_mut: bool,
        ty: Option<rue_rir::RirTypeSyntaxRef>,
        init: InstRef,
        iter_elem: bool,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Validate the type annotation, if any. Inference treats an
        // unresolvable annotation name as "no constraint" and silently falls
        // back to the initializer's type, so `let x: zzz_bogus = 5;` used to
        // compile (RUE-155). Resolve the name here so unknown annotations get
        // the same E0204 as signature positions. Comptime type variables
        // (e.g. `let P = Pair(i32); let p: P = ...`) and substituted type
        // parameters live in ctx, not in the type tables, so resolution goes
        // through `resolve_type_with_ctx`, which consults those local comptime
        // bindings at every level of a composite annotation — a scalar `P`, and
        // an inner element/pointee of `[P; 2]` / `ptr const P` alike (RUE-263).
        let annotation_type = if let Some(ty_sym) = ty {
            // A slice type `[T]` is second-class (ADR-0037, ADR-0043, RUE-322):
            // it may only name a function parameter, never a `let` local, so a
            // `let s: [i32] = ...` binding would let the view escape its
            // argument scope (E0489).
            self.reject_slice_escape(ty_sym, span, ErrorKind::SliceEscapesScope)?;
            Some(self.resolve_rir_type_with_ctx(ty_sym, span, ctx)?)
        } else {
            None
        };

        // A resolved enum annotation is the expected type for the initializer.
        // A fallible intrinsic uses this to validate that
        // `let x: Option(T) = @read_line()` names its exact registry-installed
        // result (RUE-6); context does not select the intrinsic's nominal.
        // Narrow to enums so unrelated `let`s are unaffected.
        let prev_expected = ctx.expected_type.take();
        if let Some(annot) = annotation_type {
            // A `str` annotation is the expected type for the initializer so a
            // string literal `"..."` materializes as a 2-word static-backed
            // `str` rather than the 3-word heap `String` (ADR-0043 Phase 3,
            // RUE-324). Enums do the same for fallible-intrinsic `Option(T)`.
            //
            // A `StrBuf` annotation is likewise the expected type so a string
            // literal materializes directly as the 3-word `StrBuf` (the
            // literal-backed `cap == 0` representation) rather than defaulting
            // to `str` (RUE-944). Without this the binding's storage would be a
            // 2-word `str` slot whenever inference could not pin the literal —
            // e.g. `let mut a: S = "x";` passed to a generic `inout T` parameter,
            // where `T` is unresolvable during constraint generation so no
            // literal-constraining `Equal(lit, StrBuf)` is emitted. The concrete
            // `inout StrBuf` path pins the literal through its argument
            // constraint and already saw `StrBuf`; this makes the binding's
            // storage `StrBuf` at the source so both paths agree.
            if annot.is_enum() || self.is_str_like(annot) || self.is_strbuf(annot) {
                ctx.expected_type = Some(annot);
            }
        } else if let Some(inferred) = ctx.resolved_type_of(init)
            && self.is_str_struct(inferred)
        {
            // An unannotated literal-derived initializer
            // defaults to the canonical first-class `str`. Propagate that HM
            // result while materializing nested branch, match, aggregate, and
            // block tails so every consumer uses the same two-slot type rather
            // than allowing an inner literal to fall back independently.
            ctx.expected_type = Some(inferred);
        }

        // Analyze the initializer. A `for`-loop element binding is a shared
        // read of the collection (spec 4.8:26): analyze the element read under
        // `byref_arg_root` so it borrows the element in place rather than
        // moving it out — a non-Copy element is then read, not consumed
        // (RUE-259), exactly as a `borrow`-mode argument would be. The root is
        // the collection variable (`a` in `a[__p]`); a `.chars()` scalar read
        // has no place root, so this is a no-op there.
        let init_outcome = if iter_elem {
            let byref_root = super::root_variable_of(self.body_rir_ref(), init);
            let prev = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
            let r = self.analyze_inst(air, init, ctx);
            ctx.byref_arg_root = prev;
            r
        } else {
            self.analyze_inst(air, init, ctx)
        };
        // Restore the previous expected type before propagating any error so it
        // never leaks into a sibling statement.
        ctx.expected_type = prev_expected;
        let init_result = init_outcome?;
        let var_type = init_result.ty;

        // Two-types model (ADR-0043, RUE-386): a `str`-annotated binding, or one
        // whose initializer is itself a first-class `str` slot, must not bind a
        // buffer (`StrBuf`/`Str(N)`) or a borrowed `str` view — either would let
        // the value escape past its backing storage as a first-class `str`. The
        // check covers both an explicit `let s: str = <buffer>` (annotation is
        // `str`; the annotation is otherwise unenforced here since `var_type`
        // comes from the initializer) and `let s = <view>` (no annotation).
        if annotation_type.is_some_and(|a| self.is_str_struct(a)) || self.is_str_struct(var_type) {
            self.reject_non_first_class_str(init, var_type, FirstClassStrSite::Binding, span, ctx)?;
        }

        // A plain `let` cannot bind an accessor result (ADR-0062): the
        // borrowed place's extent is the enclosing full expression, so a
        // binding would outlive the loan. A wildcard `let _` does not bind
        // and is handled as an ordinary (rejected-if-owning) read above.
        if name.is_some() {
            self.reject_accessor_result_escape(init, AccessorEscapeSite::Let, span, ctx)?;
        }

        // If name is None, this is a wildcard pattern `_` that discards the value.
        // `let _ = <expr>;` (and `let _: T = <expr>;`) is a discard site (spec
        // 3.9:18): discarding a value that carries a linear value would
        // implicitly drop it, which linearity forbids (spec 3.8:64). Reject it
        // with the same E0478 as a bare statement-expression discard (`make();`)
        // — without this, `let _` was a soundness hole that silently dropped
        // linear values (RUE-229). Once `@drop(x)` lands (RUE-187) that is the
        // sanctioned way to discard a linear value; `let _` stays an error.
        let Some(name) = name else {
            self.reject_discarded_linear_value(var_type, init)?;
            return Ok(AnalysisResult::with_continues(
                init_result.air_ref,
                Type::UNIT,
                init_result.continues,
            ));
        };

        // Special case: comptime type variables
        // When a variable is assigned a comptime type value (e.g., `let P = make_type()`),
        // we store the type in comptime_type_vars instead of creating a runtime variable.
        // This allows the variable to be used as a type annotation later (e.g., `let p: P = ...`),
        // scoped to the binding's block like any other `let` (RUE-530).
        if var_type == Type::COMPTIME_TYPE {
            // Extract the type value from the TypeConst instruction
            let inst = air.get(init_result.air_ref);
            if let AirInstData::TypeConst(ty) = &inst.data {
                ctx.bind_comptime_type_var(name, *ty);
                // Return Unit - no runtime code is generated for comptime type bindings
                let nop_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span,
                });
                return Ok(AnalysisResult::new(nop_ref, Type::UNIT));
            }
            // The analysis path didn't fold this init to a `TypeConst`. One case
            // is a cross-module zero-arg `-> type` call — `let L = m.Foo()` —
            // which method-call analysis leaves as a runtime call even though HM
            // typed it `COMPTIME_TYPE` (RUE-696; a call WITH a type argument, or
            // a same-file call, already folds via analyze_call). Re-evaluate the
            // init expression at comptime; if it reduces to a type value, bind it
            // like the `TypeConst` case above. The reduction is idempotent
            // (structural dedup), and `for_analysis` supplies the defining file
            // so a module-qualified `-> type` member resolves.
            let folded = {
                let mut env = super::super::comptime_eval::ComptimeEnv::for_analysis(ctx);
                self.eval_const_expr(init, &mut env)
            };
            if let Ok(Some(ConstValue::Type(ty))) = folded {
                ctx.bind_comptime_type_var(name, ty);
                let nop_ref = air.add_inst(AirInst {
                    data: AirInstData::UnitConst,
                    ty: Type::UNIT,
                    span,
                });
                return Ok(AnalysisResult::new(nop_ref, Type::UNIT));
            }

            // Otherwise it genuinely can't be stored at runtime.
            let name_str = self.body_interner().resolve(&name);
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "cannot store type value in variable '{}' at runtime; \
                         type values only exist at compile time",
                        name_str
                    ),
                },
                span,
            ));
        }

        // Check if @allow(unused_variable) directive is present
        let directives = self.body_rir_ref().directives(directives);
        let allow_unused = self.has_allow_directive(directives.iter(), "unused_variable");

        // Allocate slots
        let num_slots = self.require_layout_slots(var_type, span)?;
        let slot = self.reserve_frame_slots(&mut ctx.next_slot, num_slots, span)?;

        // A `for`-loop element binder over a NON-Copy collection aliases an
        // element the collection still owns and drops (spec 4.8:26): mark its
        // slot as a non-owning borrow so drop elaboration does not drop it too
        // (which would double-free the element, RUE-259). A Copy binder owns a
        // trivially-droppable copy, so it needs no marker.
        if iter_elem && !self.is_type_copy(var_type) {
            air.add_borrow_slot(slot);
        }

        // Register the variable
        ctx.insert_local(
            name,
            LocalVar {
                slot,
                ty: var_type,
                is_mut,
                span,
                allow_unused,
            },
        );

        // Emit StorageLive to mark the slot as live
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty: var_type,
            span,
        });

        // Emit the alloc instruction
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot,
                init: init_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Return a block containing both StorageLive and Alloc
        let block_ref = air.add_block(&[storage_live_ref], alloc_ref, Type::UNIT, span)?;
        Ok(AnalysisResult::with_continues(
            block_ref,
            Type::UNIT,
            init_result.continues,
        ))
    }

    /// Materialize a constant's evaluated value as an AIR instruction.
    ///
    /// Negative integers are sign-extended into the u64 payload (two's
    /// complement), matching how comptime-block results are emitted.
    pub(crate) fn materialize_const_value(
        &mut self,
        ctx: &mut AnalysisContext,
        value: ConstValue,
        ty: Type,
        anchor: Option<rue_rir::RirStructuralAnchor>,
        span: Span,
    ) -> CompileResult<(AirInstData, Type)> {
        Ok(match value {
            ConstValue::Integer(v) => (AirInstData::Const(v as u64), ty),
            ConstValue::Bool(b) => (AirInstData::BoolConst(b), Type::BOOL),
            ConstValue::Unit => (AirInstData::UnitConst, Type::UNIT),
            ConstValue::Function(_) => unreachable!(
                "function-valued constants are callable aliases and must not be materialized"
            ),
            ConstValue::Type(t) => (AirInstData::TypeConst(t), Type::COMPTIME_TYPE),
            // A string constant materializes exactly like an inline string
            // literal: its content joins the function's local string table
            // and lowers to a `.rodata`-backed `str` value (RUE-957).
            ConstValue::String(content) => {
                let content = self.body_interner().resolve(&content.spur()).to_string();
                let anchor = anchor.ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(
                            "named string constant use has no stable RIR occurrence anchor"
                                .to_string(),
                        ),
                        span,
                    )
                })?;
                let local_id = ctx.add_local_read_only_data(content, anchor);
                (AirInstData::StringConst(local_id), ty)
            }
        })
    }

    /// Analyze a variable reference.
    pub(super) fn analyze_var_ref(
        &mut self,
        air: &mut Air,
        name: Spur,
        atom_anchor: Option<rue_rir::RirStructuralAnchor>,
        span: Span,
        // The Hindley-Milner-resolved type of this reference, if known. Used
        // to recover the declared width of a captured comptime value parameter
        // (a `comptime n: u8` reference), whose `ConstValue` carries only the
        // integer magnitude, not its type (RUE-216).
        resolved_ty: Option<Type>,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // An ordinary value read is a shared use of the root. It cannot
        // overlap an exclusive accessor result in the same full expression.
        if ctx.byref_arg_root != Some(name) {
            self.reject_accessor_shared_loan_conflict(name, "by a shared read", span, ctx)?;
            ctx.expression_shared_reads.push((name, span));
        }
        // Check if it's a parameter — but a `let` that shadows the parameter
        // rebinds the name to a new local, and that local wins for all later
        // reads (spec 5.1:10, RUE-278). A same-named local can only exist by
        // shadowing, so its presence means "resolve as local" (handled below).
        if !ctx.locals.contains_key(&name) {
            if let Some(param_info) = ctx.param(name) {
                let ty = param_info.ty;
                let name_str = self.body_interner().resolve(&name);

                // Check if this parameter has been moved
                if let Some(move_state) = ctx.moved_vars.get(&name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span)
                        .with_help(super::borrow_instead_of_move_help(name_str)));
                    }
                }

                // Handle move semantics based on parameter mode.
                // A use as a by-ref call argument is a borrow, not a move
                // (`analyze_call_args_coerced` sets `byref_arg_root`), so it neither
                // marks the parameter moved nor counts as moving out of it. This is
                // what permits forwarding an inout parameter: `f(inout v)` inside
                // `fn g(inout v: T)`.
                let is_byref_arg_use = ctx.byref_arg_root == Some(name);
                let mut moves_out = false;
                if !self.is_type_copy(ty) {
                    match param_info.mode {
                        RirParamMode::Normal => {
                            if !is_byref_arg_use {
                                self.reject_move_of_call_loaned_root(name, span, ctx)?;
                                ctx.moved_vars
                                    .entry(name)
                                    .or_default()
                                    .mark_path_moved(&[], span);
                                self.record_completed_exclusive_use(name, span, ctx);
                                moves_out = true;
                            }
                        }
                        RirParamMode::Inout => {
                            // Moving out of an inout parameter would leave the
                            // CALLER's variable moved-from after the call returns,
                            // so it is rejected outright (RUE-127).
                            if !is_byref_arg_use {
                                return Err(move_out_of_inout_error(name_str, span));
                            }
                        }
                        RirParamMode::Borrow => {
                            // A by-ref argument use re-borrows the parameter:
                            // `f(borrow v)` inside `fn g(borrow v: T)` is a sound
                            // read-only re-borrow and is allowed (RUE-143).
                            // `f(inout v)` — a mutable view of read-only memory —
                            // was already rejected by call-argument analysis
                            // (E0428), so a by-ref use reaching here is borrow-mode.
                            // Anything else moves out of the borrow: rejected.
                            if !is_byref_arg_use {
                                let name_str = self.body_interner().resolve(&name);
                                return Err(CompileError::new(
                                    ErrorKind::MoveOutOfBorrow {
                                        variable: name_str.to_string(),
                                    },
                                    span,
                                ));
                            }
                        }
                    }
                }

                let mut air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span,
                });
                if moves_out {
                    // Export the move to drop elaboration: the callee-side drop
                    // of this parameter is suppressed on paths where its value
                    // moved out (RUE-61).
                    air_ref = air.add_inst(AirInst {
                        data: AirInstData::MarkMoved {
                            value: air_ref,
                            slot: param_info.abi_slot,
                            is_param: true,
                            place: None,
                        },
                        ty,
                        span,
                    });
                }
                return Ok(AnalysisResult::new(air_ref, ty));
            }
        }

        // Look up the variable in locals
        let name_str = self.body_interner().resolve(&name).to_owned();

        // Check if this is a local variable first
        if let Some(local) = ctx.locals.get(&name) {
            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been moved
            if let Some(move_state) = ctx.moved_vars.get(&name) {
                if let Some(moved_span) = move_state.is_any_part_moved() {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(&name_str)));
                }
            }

            // If type is not Copy, mark as moved — unless this use is a by-ref
            // call argument, which borrows the variable rather than moving it.
            let moves_out = !self.is_type_copy(ty) && ctx.byref_arg_root != Some(name);
            // A `for`-loop element binder over a non-Copy collection is a
            // non-owning shared borrow of an element the collection still owns
            // (spec 4.8:26): reading it is fine, but moving it out would let
            // the new owner AND the collection both drop the element
            // (double-free), so a move is rejected like moving out of a
            // `borrow` parameter (RUE-259).
            if moves_out && air.is_borrow_slot(slot) {
                return Err(CompileError::new(
                    ErrorKind::MoveOutOfBorrow {
                        variable: name_str.to_string(),
                    },
                    span,
                ));
            }
            if moves_out {
                self.reject_move_of_call_loaned_root(name, span, ctx)?;
                ctx.moved_vars
                    .entry(name)
                    .or_default()
                    .mark_path_moved(&[], span);
                self.record_completed_exclusive_use(name, span, ctx);
            }

            // Mark variable as used
            ctx.used_locals.insert(name);

            // Load the variable
            let mut air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span,
            });
            if moves_out {
                // Export the move to drop elaboration so the scope-exit drop
                // of this slot is suppressed on paths where its value moved
                // out (RUE-61).
                air_ref = air.add_inst(AirInst {
                    data: AirInstData::MarkMoved {
                        value: air_ref,
                        slot,
                        is_param: false,
                        place: None,
                    },
                    ty,
                    span,
                });
            }
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if it's a comptime type variable (e.g., `let P = Point();`)
        // These are stored in comptime_type_vars, not in locals
        if let Some(&ty) = ctx.comptime_type_vars.get(&name) {
            // Comptime type vars produce TypeConst instructions
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(ty),
                ty: Type::COMPTIME_TYPE,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
        }

        // Check if it's a comptime value variable (e.g., captured `comptime N: i32`)
        // When an anonymous struct method captures comptime parameters from its enclosing function,
        // references to those parameters are resolved here and emitted as const instructions.
        if let Some(const_value) = ctx.comptime_value_vars.get(&name) {
            match const_value {
                ConstValue::Integer(val) => {
                    // Emit the const with the parameter's declared width. The
                    // `ConstValue` carries only the integer magnitude, so the
                    // type comes from the HM-resolved type of this reference
                    // (a captured `comptime n: u8` reference resolves to u8).
                    // Falling back to i32 when no integer type was resolved
                    // preserves the historical behavior for the untyped case
                    // (RUE-216).
                    let ty = match resolved_ty {
                        Some(t) if t.is_integer() => t,
                        _ => Type::I32,
                    };
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
                ConstValue::Bool(val) => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(*val as u64),
                        ty: Type::BOOL,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::BOOL));
                }
                ConstValue::Type(ty) => {
                    // If someone captured a type value, treat it like a type const
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::TypeConst(*ty),
                        ty: Type::COMPTIME_TYPE,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
                }
                ConstValue::Function(_) => {
                    return Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: "a function reference".to_string(),
                        },
                        span,
                    ));
                }
                // No comptime parameter has a string type, so a captured
                // string value never occurs (RUE-957).
                ConstValue::String(_) => {
                    return Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: "a captured string value".to_string(),
                        },
                        span,
                    ));
                }
                ConstValue::Unit => {
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(0),
                        ty: Type::UNIT,
                        span,
                    });
                    return Ok(AnalysisResult::new(air_ref, Type::UNIT));
                }
            }
        }

        // Check if it's a module binding declared in this file (`const math =
        // @import("math")`). Module bindings are per-file scoped (RUE-113),
        // so the lookup is keyed by the reference's own file and takes
        // precedence over file-local value constants.
        let module_binding = self.call_facts().call_module_binding(span.file_id, name);
        if let Some(binding) = module_binding {
            self.record_body_named_dependency(
                super::super::NamedConstDependencyTargetEvent::ModuleBinding {
                    file: span.file_id.index(),
                    name: name_str.to_string(),
                },
            );
            let ty = binding.ty;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::TypeConst(ty),
                ty,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if it's a value constant (e.g., `const VALUE: i32 = -42;`).
        // Module-typed constants never appear here: module bindings AND
        // aliases (`const m2 = std.math;`) are tagged module bindings,
        // checked above. The value was evaluated once during declaration
        // gathering (RUE-171); materialize it directly — the initializer is
        // never re-analyzed at use sites.
        let const_info = self
            .call_facts()
            .call_resolve_const_info_in_file(name, span.file_id);
        if let Some(const_info) = const_info {
            // Apply the uniform privacy rule even though ordinary unqualified
            // lookup resolves in the reference file (spec 10.3:1, 10.3:7).
            self.check_unqualified_visibility(
                "constant",
                &name_str,
                const_info.span.file_id,
                const_info.is_pub,
                span,
            )?;
            self.record_body_named_dependency(
                super::super::NamedConstDependencyTargetEvent::ValueConst {
                    file: const_info.span.file_id.index(),
                    name: name_str.to_string(),
                },
            );
            if matches!(const_info.value, ConstValue::Function(_)) {
                return Err(CompileError::new(
                    ErrorKind::ConstExprNotSupported {
                        expr_kind: "a function reference".to_string(),
                    },
                    span,
                ));
            }
            let (data, ty) = self.materialize_const_value(
                ctx,
                const_info.value,
                const_info.ty,
                atom_anchor,
                span,
            )?;
            let air_ref = air.add_inst(AirInst { data, ty, span });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // Check if this is a type name (for comptime type parameters)
        // Try to resolve it as a type - if successful, emit a TypeConst instruction
        match self.resolve_type(name, span) {
            Ok(resolved_type) => {
                // This is a type name being used as a value (e.g., `i32` passed to `comptime T: type`)
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(resolved_type),
                    ty: Type::COMPTIME_TYPE,
                    span,
                });
                return Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE));
            }
            // The name IS a known type, but a private one from another
            // directory (RUE-183): report the privacy error rather than
            // falling through to a misleading "undefined variable".
            Err(e) if matches!(e.kind, ErrorKind::PrivateUnqualifiedAccess(_)) => {
                return Err(e);
            }
            // Any other resolution failure: not a type name, keep falling
            // through to the undefined-variable error below.
            Err(_) => {}
        }

        // Not a parameter, local, type, or constant - undefined variable
        Err(CompileError::new(
            ErrorKind::UndefinedVariable(name_str.to_string()),
            span,
        ))
    }

    /// Build E0203 for an immutable by-value parameter with a receiver-aware hint.
    pub(crate) fn immutable_param_assign_error(
        &self,
        name_str: &str,
        ty: Type,
        span: Span,
    ) -> CompileError {
        let error = CompileError::new(ErrorKind::AssignToImmutable(name_str.to_string()), span);
        if name_str == "self" {
            error.with_help(
                "consider declaring the receiver mutable: `mut self` (mutates the callee's copy \
                 only), or `inout self` to write back to the caller",
            )
        } else {
            error.with_help(format!(
                "consider making parameter `{}` inout: `inout {}: {}`",
                name_str,
                name_str,
                ty.safe_name_with_pool(Some(self.body_type_pool()))
            ))
        }
    }

    /// Analyze an assignment.
    fn analyze_assign(
        &mut self,
        air: &mut Air,
        name: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let name_str = self.body_interner().resolve(&name);

        // Reassigning a collection that an enclosing `for` loop is iterating
        // mutates a shared-borrowed value (spec 4.8:26, RUE-233) — E0428, just
        // like assigning through an explicit `borrow` parameter.
        if ctx.iter_borrows.contains(&name) {
            return Err(CompileError::new(
                ErrorKind::MutateBorrowedValue {
                    variable: name_str.to_string(),
                },
                span,
            ));
        }

        // Check if it's a parameter (for inout params) — unless a `let mut`
        // shadowed it with a local of the same name, in which case the
        // assignment targets that mutable local, not the parameter (RUE-278).
        // Without this guard, `let mut x = x; x = x + 1;` wrongly reports E0203
        // against the immutable parameter with a bogus "make it inout" hint.
        if !ctx.locals.contains_key(&name) {
            if let Some(param_info) = ctx.param(name) {
                // Inout params and `mut self` (a mutable by-value receiver
                // binding) can be assigned to.
                match param_info.mode {
                    RirParamMode::Normal if param_info.is_mut => {
                        // `mut self` mutates the callee's copy only.
                    }
                    RirParamMode::Normal => {
                        return Err(self.immutable_param_assign_error(
                            name_str,
                            param_info.ty,
                            span,
                        ));
                    }
                    RirParamMode::Inout => {
                        // Inout parameters can be assigned to
                    }
                    RirParamMode::Borrow => {
                        return Err(CompileError::new(
                            ErrorKind::MutateBorrowedValue {
                                variable: name_str.to_string(),
                            },
                            span,
                        ));
                    }
                }

                let abi_slot = param_info.abi_slot;
                let param_ty = param_info.ty;

                // `inout str` is a second-class exclusive view over a caller's
                // concrete `StrBuf`/`Str(N)` storage (spec 3.7:58-60), not a
                // first-class header that can be rebound. Treating a whole
                // assignment as an ordinary ParamStore is representation-
                // unsafe: the unconstrained literal defaults to 3-word
                // StrBuf, even when the caller supplied a 2-word Str(N), and
                // the generated drop/store reads and writes past the caller's
                // value (RUE-641). Byte-level mutation through the exclusive
                // view remains distinct from rebinding the view itself.
                if self.is_str_struct(param_ty) {
                    return Err(
                        CompileError::new(ErrorKind::StrViewReassignment, span).with_help(
                            "use an exact `inout Str(N)` or `inout StrBuf` parameter when \
                             whole-value replacement is intended",
                        ),
                    );
                }

                // A direct fixed-string literal is contextually typed by its
                // exact destination capacity (spec 3.7:50). Keep the context
                // deliberately at the literal boundary: applying it to an
                // arbitrary RHS would leak Str(N) into call/intrinsic operands
                // (for example the StrBuf message in a never-returning call).
                // Non-literal values retain their own type and are checked
                // exactly below.
                let contextual_fixed_literal = self.is_str_fixed_struct(param_ty)
                    && matches!(
                        &self.body_rir_ref().get(value).data,
                        InstData::StringConst { .. }
                    );
                let value_result = if contextual_fixed_literal {
                    let prev_expected = ctx.expected_type.replace(param_ty);
                    let result = self.analyze_inst(air, value, ctx);
                    ctx.expected_type = prev_expected;
                    result?
                } else {
                    self.analyze_inst(air, value, ctx)?
                };
                self.reject_accessor_result_escape(value, AccessorEscapeSite::Store, span, ctx)?;
                self.reject_accessor_loan_conflict(name, "by assignment", span, ctx)?;

                // ParamStore has no coercion or conversion step: codegen drops
                // and copies according to the RHS type. Require semantic type
                // identity before emitting it so equal source/target layout is
                // a proved invariant, not an assumption. Never/Error retain
                // their usual recovery coercions and cannot execute a store.
                if !self.types_equivalent(value_result.ty, param_ty)
                    && !value_result.ty.is_never()
                    && !value_result.ty.is_error()
                    && !param_ty.is_error()
                {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: param_ty.safe_name_with_pool(Some(self.body_type_pool())),
                            found: value_result
                                .ty
                                .safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        self.body_rir_ref().get(value).span,
                    ));
                }

                // RUE-387: reassigning an `inout` parameter whose type carries
                // a linear value would drop the caller's live value implicitly.
                // An `inout` binding can never be proven moved-out, so this is
                // always ill-formed (E0494).
                let discharged = self.place_linear_discharged(param_ty, name, &[], ctx);
                self.check_linear_overwrite(param_ty, discharged, true, span)?;

                // Assignment to a parameter resets its move state
                ctx.moved_vars.remove(&name);

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::ParamStore {
                        param_slot: abi_slot,
                        value: value_result.air_ref,
                    },
                    ty: Type::UNIT,
                    span,
                });
                if value_result.continues {
                    self.record_completed_exclusive_use(name, span, ctx);
                }
                return Ok(AnalysisResult::with_continues(
                    air_ref,
                    Type::UNIT,
                    value_result.continues,
                ));
            }
        }

        // Look up local variable
        let local = ctx
            .locals
            .get(&name)
            .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

        // Check mutability
        if !local.is_mut {
            return Err(CompileError::new(
                ErrorKind::AssignToImmutable(name_str.to_string()),
                span,
            )
            .with_label("variable declared as immutable here", local.span)
            .with_help(format!(
                "consider making `{}` mutable: `let mut {}`",
                name_str, name_str
            )));
        }

        let slot = local.slot;
        let local_ty = local.ty;

        // Analyze the value. When the target is a `str` (ADR-0043 Phase 3,
        // RUE-324), supply it as the expected type so a string literal RHS
        // materializes as a 2-word `str` rather than a 3-word `String` (which
        // would corrupt the 2-slot local); this is what makes `let mut s: str;
        // s = "hi";` reassignment sound.
        let value_result = if self.is_str_like(local_ty) {
            let prev_expected = ctx.expected_type.replace(local_ty);
            let r = self.analyze_inst(air, value, ctx);
            ctx.expected_type = prev_expected;
            r?
        } else {
            self.analyze_inst(air, value, ctx)?
        };
        self.reject_accessor_result_escape(value, AccessorEscapeSite::Store, span, ctx)?;
        self.reject_accessor_loan_conflict(name, "by assignment", span, ctx)?;

        // RUE-387: overwriting a local that still holds a live linear value
        // would drop it implicitly. Legal only when the whole variable was
        // proven moved-out on every path (reinit-after-move idiom) — evaluated
        // on the post-RHS move state so `x = f(x)` counts as a discharge.
        let discharged = self.place_linear_discharged(local_ty, name, &[], ctx);
        self.check_linear_overwrite(local_ty, discharged, false, span)?;
        // Two-types model (ADR-0043, RUE-386): reassigning a first-class `str`
        // local must store a first-class `str`, not a buffer or a borrowed view;
        // truncating a 3-word `StrBuf` into the 2-word slot would leave a
        // dangling pointer.
        if self.is_str_struct(local_ty) {
            self.reject_non_first_class_str(
                value,
                value_result.ty,
                FirstClassStrSite::Binding,
                span,
                ctx,
            )?;
        }

        // Assignment to a mutable variable resets its move state.
        ctx.moved_vars.remove(&name);

        // Emit store instruction
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Store {
                slot,
                value: value_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        if value_result.continues {
            self.record_completed_exclusive_use(name, span, ctx);
        }
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            value_result.continues,
        ))
    }

    /// Reject a move (full or partial) whose root is a by-ref parameter.
    ///
    /// Moving a non-Copy value out of an `inout` or `borrow` parameter would
    /// leave the CALLER's variable (partially) moved-from after the call
    /// returns, so both are rejected outright (RUE-127); reinitialization
    /// before exit is not tracked yet.
    fn reject_move_out_of_byref_param(
        &self,
        root_var: Spur,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<()> {
        // A `let` shadowing the by-ref parameter rebinds the name to an owned
        // local; moving out of that local is fine, so the parameter rule no
        // longer applies (RUE-278).
        if ctx.locals.contains_key(&root_var) {
            return Ok(());
        }
        if let Some(param_info) = ctx.param(root_var) {
            match param_info.mode {
                RirParamMode::Inout => {
                    return Err(move_out_of_inout_error(
                        self.body_interner().resolve(&root_var),
                        span,
                    ));
                }
                RirParamMode::Borrow => {
                    return Err(CompileError::new(
                        ErrorKind::MoveOutOfBorrow {
                            variable: self.body_interner().resolve(&root_var).to_string(),
                        },
                        span,
                    ));
                }
                RirParamMode::Normal => {}
            }
        }
        Ok(())
    }

    /// Whether `struct_id` names a struct **declared** `linear`, as opposed to
    /// one that is linear only because a field carries a linear value
    /// (infectious linearity, spec 3.8:58).
    ///
    /// The pool's `StructDef::declared_linear` is authoritative: it records
    /// the source declaration verbatim and the containment-facts join never
    /// touches it, while `StructDef::is_linear` is the transitive join and so
    /// cannot tell the two apart on its own.
    ///
    /// The distinction is load-bearing (RUE-1591): whole-value destructuring
    /// consumption (3.8:33, 3.8:74 — the obligation belongs to the VALUE
    /// regardless of what its fields hold) applies to a declared-`linear`
    /// struct only. An infectious carrier follows the ordinary partial-move
    /// model of 3.8:22/3.8:28.
    pub(crate) fn struct_declared_linear(&self, struct_id: StructId) -> bool {
        let def = self.body_type_pool().struct_def(struct_id);
        // The diagnostics helper `infectious_linear_reason` names a culprit
        // field exactly when the join — and not the declaration — is what
        // made the struct linear, so the old derivation must agree with the
        // declared bit for every complete struct.
        debug_assert_eq!(
            def.declared_linear,
            def.is_linear && self.infectious_linear_reason(struct_id).is_none(),
            "declared_linear bit disagrees with the infectious-reason derivation for '{}'",
            def.name,
        );
        def.declared_linear
    }

    /// The projection depth of the value a declared-`linear` field access
    /// destructures: `trace.projections[..depth]` names the consumed place,
    /// and `trace.projections[depth]` is the field access that destructures
    /// it (RUE-1632).
    ///
    /// Destructuring consumption (3.8:33) consumes the destructured VALUE,
    /// which is a place — `h.arr[0]`, `h.a`, or the root binding itself —
    /// not the root binding in every case. Scoping it to the root discharged
    /// every sibling place's linear obligation along with it (RUE-1632).
    ///
    /// The SHALLOWEST declared-`linear` container along the chain wins:
    /// reaching a deeper one means projecting through the shallow one, which
    /// destructures (and so consumes) that outer value wholly, residue
    /// included. Levels above it are not declared linear, so they follow the
    /// ordinary partial-move model (3.8:22, RUE-1591) and keep the
    /// obligations of their other sub-places.
    ///
    /// Only called once the last container is known to be declared linear, so
    /// a level always exists; depth 0 (consume the root) is the fallback.
    fn declared_linear_destructure_depth(&self, trace: &PlaceTrace) -> usize {
        let mut container = trace.base_type;
        for (depth, proj) in trace.projections.iter().enumerate() {
            if container
                .as_struct()
                .is_some_and(|id| self.struct_declared_linear(id))
            {
                return depth;
            }
            container = proj.result_type;
        }
        0
    }

    /// Reject a field access that consumes (destructures) a **declared**
    /// `linear` struct when the destructure would implicitly drop a
    /// *different* field that itself carries a linear value (E0474, spec
    /// 3.8:60, RUE-40).
    ///
    /// Destructuring consumption (spec 3.8:33) extracts the accessed leaf
    /// field and drops everything else in the value. That implicit drop must
    /// not lose a linear value. Every struct level along the projection
    /// chain is checked: at each level, all fields other than the projected
    /// one are dropped.
    ///
    /// Only levels whose struct is declared `linear` destructure this way
    /// (RUE-1591). A field access on an *infectious* carrier is an ordinary
    /// partial move: the siblings stay Owned and their residue is dropped by
    /// the scope-exit walk, so a linear sibling is caught by the residual
    /// must-consume check ([`Self::residual_linear_place`], core §5.6)
    /// instead of here — that is what lets `sink(c.inner)` keep a Copy
    /// sibling readable while `c.tag` alone still fails.
    fn reject_linear_destructure_dropping_linear_field(
        &self,
        trace: &PlaceTrace,
        span: Span,
    ) -> CompileResult<()> {
        let mut current_ty = trace.base_type;
        for proj in &trace.projections {
            if let AirProjection::Field {
                struct_id,
                field_index,
            } = proj.proj
                && self.struct_declared_linear(struct_id)
            {
                let def = self.body_type_pool().struct_def(struct_id);
                for (i, field) in def.fields.iter().enumerate() {
                    if i as u32 != field_index && self.type_carries_linear(field.ty) {
                        let accessed = def.fields[field_index as usize].name.clone();
                        let err = CompileError::new(
                            ErrorKind::LinearFieldDroppedByDestructure(Box::new(
                                rue_error::LinearFieldDroppedByDestructureError {
                                    struct_name: def.name.to_string(),
                                    accessed,
                                    dropped: field.name.clone(),
                                },
                            )),
                            span,
                        )
                        .with_help(
                            "destructuring consumption extracts one field and drops \
                             the rest; access the linear field instead and consume \
                             its value, or consume the whole value by passing it to \
                             a function",
                        );
                        return Err(self.attach_infectious_linear_note(err, current_ty));
                    }
                }
            }
            current_ty = proj.result_type;
        }
        Ok(())
    }

    /// Reject moving a field out of a value whose struct type has a
    /// destructor (RUE-158, the spirit of Rust's E0509).
    ///
    /// The destructor always runs on the *whole* value when it is dropped:
    /// it would observe the moved-out field (a use-after-free for heap
    /// fields), and the automatic field cleanup after the destructor would
    /// drop the field a second time. Inside `drop fn T(self)` this same rule
    /// rejects `self.field` moves — `self` has type `T`, which has the very
    /// destructor being defined (whole-`self` moves are E0442).
    ///
    /// Every container along the projection chain is checked (`o.a.b` moves
    /// out of both `o` and `o.a`), mirroring Rust: a deep move leaves every
    /// enclosing value partially moved. Whole-value moves and `borrow`/
    /// `inout` access of fields (RUE-143) stay legal and never reach here.
    fn reject_field_move_out_of_destructor_type(
        &self,
        trace: &PlaceTrace,
        span: Span,
    ) -> CompileResult<()> {
        for (i, proj_info) in trace.projections.iter().enumerate() {
            let container = if i == 0 {
                trace.base_type
            } else {
                trace.projections[i - 1].result_type
            };
            let Some(struct_id) = container.as_struct() else {
                continue;
            };
            let struct_def = self.body_type_pool().struct_def(struct_id);
            if struct_def.destructor.is_none() {
                continue;
            }
            let field_name = proj_info
                .field_name
                .map(|s| self.body_interner().resolve(&s).to_string())
                .unwrap_or_default();
            let mut err = CompileError::new(
                ErrorKind::MoveFieldOutOfDestructorType {
                    struct_name: struct_def.name.to_string(),
                    field_name: field_name.clone(),
                },
                span,
            )
            .with_label(format!("field `{field_name}` is moved out here"), span)
            .with_note(format!(
                "the destructor for '{}' runs on the whole value when it is dropped: it \
                 would observe the moved-out field, and the automatic field cleanup after \
                 the destructor would drop `{field_name}` a second time",
                struct_def.name
            ))
            .with_help(format!(
                "borrow the field instead (`borrow value.{field_name}`), or move the whole value"
            ));
            if let Some(drop_span) = self.destructor_span(struct_id).as_ref() {
                err = err.with_label(
                    format!("destructor for '{}' is defined here", struct_def.name),
                    *drop_span,
                );
            }
            return Err(err);
        }
        Ok(())
    }

    /// Analyze a field access.
    ///
    /// Addressable field chains become a `PlaceRead`; computed bases are first
    /// materialized in a temporary place.
    pub(crate) fn analyze_field_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        field: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // First, check if the base is a module access (special case, not a place)
        // We need to peek at the base type to detect module.Type access patterns.
        let base_inst = {
            let source = self.body_rir_ref().get(base);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };
        if let InstData::VarRef { name, .. } = &base_inst.data {
            if !self.is_runtime_value_binding(*name, ctx) {
                if let Some(result) =
                    self.try_analyze_dotted_enum_variant(air, *name, field, span, ctx)?
                {
                    return Ok(result);
                }
            }

            // Check if this VarRef refers to a module
            if let Some(local) = ctx.locals.get(name) {
                if let Some(module_id) = local.ty.as_module() {
                    // This is module.Member access - handle specially.
                    // Member access is a use of the binding (`let m =
                    // @import(..); m.ANSWER` must not warn about `m`).
                    ctx.used_locals.insert(*name);
                    return self.analyze_module_type_member_access(
                        air,
                        module_id,
                        field,
                        super::const_use_anchor_of(self.body_rir_ref(), base),
                        span,
                        ctx,
                    );
                }
            }
        }

        // Module-qualified enum-variant path: `module.Enum.Variant` (RUE-488).
        // The base is `module.Enum` (a field access), whose module member is an
        // enum type; `.Variant` selects the variant.
        if let InstData::FieldGet {
            base: module_ref,
            field: type_name,
        } = base_inst.data
        {
            if let Some(result) = self.try_analyze_module_dotted_enum_variant(
                air, module_ref, type_name, field, span, ctx,
            )? {
                return Ok(result);
            }
        }

        // Try to trace this expression to a place (lvalue)
        if let Some(mut trace) = self.try_trace_place(inst_ref, air, ctx)? {
            if !trace.via_accessor && ctx.byref_arg_root != Some(trace.root_var) {
                self.reject_accessor_shared_loan_conflict(
                    trace.root_var,
                    "by a shared read",
                    span,
                    ctx,
                )?;
                ctx.expression_shared_reads.push((trace.root_var, span));
            }
            let field_type = trace.result_type();

            // Check if the root variable was fully moved (applies regardless of field type)
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.body_interner().resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(root_name)));
                }
            }

            // A field read through an array element (`xs[0].f`) must not go
            // through a moved-out element (RUE-186).
            self.check_read_through_moved_element(&trace, ctx, span)?;

            // Get struct info for move checking
            // The trace's result type is the field type, but we need the parent struct type
            // to check if it's linear. The parent is the type *before* the last projection.
            let parent_type = if trace.projections.len() > 1 {
                trace.projections[trace.projections.len() - 2].result_type
            } else {
                trace.base_type
            };

            // Whole-value destructuring consumption (3.8:33) is the model for
            // a struct DECLARED `linear` only: its obligation belongs to the
            // value itself, whatever its fields hold (3.8:74). A struct that
            // is linear merely because a field carries a linear value
            // (infectious linearity, 3.8:58) follows the ORDINARY partial-move
            // model instead (3.8:22/3.8:28, core §5.6): the accessed field
            // moves, its siblings stay Owned and readable, and the non-moved
            // residue is dropped by the scope-exit walk (3.8:60, 3.9:2). The
            // linear obligation then lives on the linear sub-place and is
            // discharged by consuming it — see `residual_linear_place`.
            // Modelling the carrier as a whole-value consumption instead both
            // leaked the siblings' drop glue and over-rejected Copy sibling
            // reads (RUE-1591). "Whole-value" is the destructured struct's
            // PLACE, which is the root binding only when the declared-linear
            // struct IS the root (RUE-1632) — see
            // `declared_linear_destructure_depth`.
            let is_declared_linear = parent_type
                .as_struct()
                .is_some_and(|id| self.struct_declared_linear(id));

            // Move checking using the trace. `move_is_partial` selects the
            // MarkMoved marker's place component: absent for a whole-slot
            // move, the accessed place for a field-path move (RUE-62,
            // RUE-157), and the destructured sub-place for a nested
            // declared-linear access (RUE-1632).
            //
            // A use as a by-ref call argument (`f(borrow o.f)`, `f(inout
            // o.f)`) borrows the place rather than moving out of it
            // (RUE-143): no move is recorded and the by-ref-param move
            // rejections don't apply — but reading through an already-moved
            // path is still a use-after-move.
            let is_byref_arg_use = ctx.byref_arg_root == Some(trace.root_var);

            // Moving a (non-Copy) field or the whole value out of a `for`-loop
            // element binder over a non-Copy collection would let the new owner
            // AND the collection both drop it (double-free): iteration is a
            // shared read (spec 4.8:26), so the binder may be read but not
            // moved out, whole-value or field-granular (RUE-259).
            if !is_byref_arg_use
                && (is_declared_linear || !self.is_type_copy(field_type))
                && matches!(trace.base, AirPlaceBase::Local(s) if air.is_borrow_slot(s))
            {
                return Err(CompileError::new(
                    ErrorKind::MoveOutOfBorrow {
                        variable: self.body_interner().resolve(&trace.root_var).to_string(),
                    },
                    span,
                ));
            }

            let mut emit_move_marker = false;
            let mut move_is_partial = false;
            // How many of the trace's projections the move marker's place
            // keeps: the whole chain names the accessed field (an ordinary
            // partial move), a strict prefix names the destructured value a
            // nested declared-`linear` access consumes (RUE-1632).
            let mut marker_depth = trace.projections.len();
            if is_byref_arg_use {
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::use_after_move_path_error(
                            self.body_interner(),
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
            } else if is_declared_linear {
                // For a declared-linear struct, field access destructures and
                // so consumes that struct's whole value (3.8:33).
                self.reject_accessor_place_move(&trace, field_type, span)?;
                self.reject_linear_destructure_dropping_linear_field(&trace, span)?;
                self.reject_move_out_of_byref_param(trace.root_var, ctx, span)?;
                self.reject_move_of_call_loaned_root(trace.root_var, span, ctx)?;

                // The consumed value is the destructured struct's PLACE. When
                // it is a sub-place of the root (`h.arr[0].v`, `h.a.v`), only
                // that place is consumed: the root's other sub-places keep
                // their own linear obligations and their drop glue (RUE-1632).
                // An unnameable place (dynamic index in the prefix) cannot be
                // recorded per-place, so it keeps the whole-root consumption.
                marker_depth = self.declared_linear_destructure_depth(&trace);
                let destructured_path = trace.prefix_field_path(marker_depth).unwrap_or_default();

                // Destructuring a sub-place uses it as a whole value, so the
                // place itself, an ancestor, or a descendant being moved makes
                // this a use-after-move (RUE-279) — at the root the full-move
                // check above already covers it.
                if !destructured_path.is_empty()
                    && let Some(state) = ctx.moved_vars.get(&trace.root_var)
                    && let Some(moved_span) = state.is_path_or_descendant_moved(&destructured_path)
                {
                    return Err(super::use_after_move_path_error(
                        self.body_interner(),
                        trace.root_var,
                        &destructured_path,
                        span,
                        moved_span,
                    ));
                }

                move_is_partial = !destructured_path.is_empty();
                ctx.moved_vars
                    .entry(trace.root_var)
                    .or_default()
                    .mark_path_moved(&destructured_path, span);
                if trace.continues {
                    self.record_completed_exclusive_use(trace.root_var, span, ctx);
                }
                emit_move_marker = true;
            } else if !self.is_type_copy(field_type) {
                // For non-linear types, check if accessing a non-Copy field
                self.reject_accessor_place_move(&trace, field_type, span)?;
                self.reject_move_out_of_byref_param(trace.root_var, ctx, span)?;
                self.reject_field_move_out_of_destructor_type(&trace, span)?;

                if trace.has_untrackable_index() {
                    return Err(CompileError::new(
                        ErrorKind::MoveOutOfIndex {
                            element_type: field_type
                                .safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        span,
                    )
                    .with_help(
                        "moving a field out through an array index requires a \
                         compile-time constant index",
                    ));
                }

                let field_path = trace.field_path();

                // Check if this field path is already moved. Moving the field
                // out as a whole value is illegal if the path itself, an
                // ancestor, OR any descendant subfield was already moved
                // (`o.inner` cannot be passed by value once `o.inner.s` moved —
                // spec 3.8, RUE-279), so check both directions here, unlike a
                // Copy leaf read below which only cares about ancestors.
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_or_descendant_moved(&field_path) {
                        return Err(super::use_after_move_path_error(
                            self.body_interner(),
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }

                // Mark this field path as moved
                self.reject_move_of_call_loaned_root(trace.root_var, span, ctx)?;
                ctx.moved_vars
                    .entry(trace.root_var)
                    .or_default()
                    .mark_path_moved(&field_path, span);
                if trace.continues {
                    self.record_completed_exclusive_use(trace.root_var, span, ctx);
                }

                // Export statically-trackable partial moves to drop elaboration
                // so the moved path's drop inside the scope-exit drop is
                // suppressed. This covers pure field paths (`o.a`, `o.a.b`,
                // RUE-62/RUE-157) and constant-index array subfields
                // (`arr[0].a`, RUE-494). Dynamic index paths were rejected
                // above because drop elaboration cannot know which element was
                // holed. Only Normal params occupy a real ABI slot that drop
                // elaboration would drop.
                let is_droppable_param_base = match trace.base {
                    AirPlaceBase::Local(_) => true,
                    AirPlaceBase::Param(_) => ctx
                        .params
                        .iter()
                        .any(|p| p.name == trace.root_var && p.mode == RirParamMode::Normal),
                    AirPlaceBase::Accessor(_) | AirPlaceBase::Indirect(_) => false,
                };
                if is_droppable_param_base {
                    emit_move_marker = true;
                    move_is_partial = true;
                }
            } else {
                // Copy fields are read, not moved — but reading one THROUGH
                // a moved ancestor (`o.f.x` after `o.f` was moved out) reads
                // memory whose owner is gone: drops are move-aware, so the
                // moved part is logically dead. `is_path_moved` checks the
                // exact path and every ancestor prefix (the full-move case
                // was already rejected above).
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::use_after_move_path_error(
                            self.body_interner(),
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
            }

            // Emit PlaceRead instruction
            let place_ref = Self::build_place_ref(air, &trace)?;
            if trace.via_accessor {
                ctx.accessor_place_refs.insert(
                    inst_ref,
                    (
                        place_ref,
                        trace.root_var,
                        field_type,
                        trace.is_root_mutable,
                        trace.continues,
                    ),
                );
            }
            let mut air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: field_type,
                span,
            });
            if emit_move_marker {
                // Export the move (the whole slot, the accessed field path, or
                // the destructured sub-place of a nested declared-`linear`
                // access — RUE-1632) to drop elaboration.
                let (slot, is_param) = match trace.base {
                    AirPlaceBase::Local(slot) => (slot, false),
                    AirPlaceBase::Param(slot) => (slot, true),
                    AirPlaceBase::Accessor(_) | AirPlaceBase::Indirect(_) => {
                        unreachable!("accessor places never receive move markers")
                    }
                };
                let marker_place = move_is_partial
                    .then(|| {
                        Self::build_move_marker_place_ref(
                            air,
                            &trace,
                            &trace.projections[..marker_depth],
                            span,
                        )
                    })
                    .transpose()?;
                air_ref = air.add_inst(AirInst {
                    data: AirInstData::MarkMoved {
                        value: air_ref,
                        slot,
                        is_param,
                        place: marker_place,
                    },
                    ty: field_type,
                    span,
                });
            }
            // Accessor guard statements (ADR-0062) run before the read.
            let air_ref = Self::finish_traced_value(air, &mut trace, air_ref, field_type, span)?;
            return Ok(AnalysisResult::with_continues(
                air_ref,
                field_type,
                trace.continues,
            ));
        }

        // Fallback: base is not a place (e.g., function call result)
        // Spill the computed value to a temporary, then use PlaceRead.
        // This handles `get_struct().field` patterns.
        let base_result = self.analyze_inst(air, base, ctx)?;
        let base_type = base_result.ty;

        // Handle module member access that wasn't caught above
        if let Some(module_id) = base_type.as_module() {
            return self.analyze_module_type_member_access(
                air,
                module_id,
                field,
                super::const_use_anchor_of(self.body_rir_ref(), base),
                span,
                ctx,
            );
        }

        let struct_id = match base_type.as_struct() {
            Some(id) => id,
            None => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: base_type.safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    span,
                ));
            }
        };

        let struct_def = self.body_type_pool().struct_def(struct_id);
        let field_name = self.body_interner().resolve(&field);
        let Some((field_index, struct_field)) = struct_def.find_field(field_name) else {
            return Err(CompileError::new(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.to_string(),
                    field_name: field_name.to_string(),
                },
                span,
            ));
        };

        let field_type = struct_field.ty;

        // Allocate a temporary slot for the computed struct value
        let num_slots = self.require_layout_slots(base_type, span)?;
        let temp_slot = self.reserve_frame_slots(&mut ctx.next_slot, num_slots, span)?;

        // Emit StorageLive for the temporary
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });

        // Emit Alloc to store the computed value
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Create PlaceRead with Field projection on the temp slot
        let place_ref = air.make_place(
            AirPlaceBase::Local(temp_slot),
            base_type,
            std::iter::once(AirProjection::Field {
                struct_id,
                field_index: field_index as u32,
            }),
        )?;
        let mut value_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: field_type,
            span,
        });

        // Projecting a non-Copy field out of this spilled temporary MOVES it
        // out of the temporary. The temporary is dropped whole at scope exit
        // (drop elaboration in rue-cfg), so without a move marker its per-field
        // drop glue would re-drop the extracted field that the new owner (e.g.
        // the `let` binding) also drops — a double free (RUE-258). Emit a
        // field-path MarkMoved on the temp slot, exactly as the named-place
        // path above does, so the temporary's scope-exit drop skips this field.
        if !self.is_type_copy(field_type) {
            value_ref = air.add_inst(AirInst {
                data: AirInstData::MarkMoved {
                    value: value_ref,
                    slot: temp_slot,
                    is_param: false,
                    place: Some(place_ref),
                },
                ty: field_type,
                span,
            });
        }

        // Note: We don't emit StorageDead here. The temporary will be cleaned up by
        // scope-based drop elaboration in the CFG builder. This is slightly conservative
        // (temp lives until scope exit rather than immediately after use) but correct.
        // A future optimization could add explicit StorageDead at the right point.
        let block_ref =
            air.add_block(&[storage_live_ref, alloc_ref], value_ref, field_type, span)?;
        Ok(AnalysisResult::with_continues(
            block_ref,
            field_type,
            base_result.continues,
        ))
    }

    /// Analyze an array index read.
    ///
    /// Addressable index chains become a `PlaceRead`; computed bases are first
    /// materialized in a temporary place.
    pub(crate) fn analyze_index_get(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        base: InstRef,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Check for constant out-of-bounds index early (before tracing)
        // We need the array type for bounds checking, so peek at the base first
        let _base_inst = self.body_rir_ref().get(base);

        // Try to trace this expression to a place (lvalue)
        if let Some(mut trace) = self.try_trace_place(inst_ref, air, ctx)? {
            if !trace.via_accessor && ctx.byref_arg_root != Some(trace.root_var) {
                self.reject_accessor_shared_loan_conflict(
                    trace.root_var,
                    "by a shared read",
                    span,
                    ctx,
                )?;
                ctx.expression_shared_reads.push((trace.root_var, span));
            }
            let elem_type = trace.result_type();

            // Reading through an index expression whose base place has been
            // moved is a use-after-move (RUE-232), exactly like a field read
            // of a moved place. A plain (Copy-element, non-byref) index read
            // reached neither move check below — the byref branch checks its
            // own path and the non-Copy branch records an element move — so
            // `moved.field[i]` after `let b = moved` slipped through. Check the
            // base place's move state here for every index read. `field_path()`
            // names this element (constant index) or the base up to a dynamic
            // index, so `is_path_moved` catches a full move of the root, a
            // moved field/element prefix, or this exact constant element,
            // without flagging a *sibling* element (`arr[1]` after `arr[0]`
            // moved has a disjoint path).
            {
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::use_after_move_path_error(
                            self.body_interner(),
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
            }

            // Get array info from the parent type (before the last projection)
            let parent_type = if trace.projections.len() > 1 {
                trace.projections[trace.projections.len() - 2].result_type
            } else {
                trace.base_type
            };

            let array_len = match parent_type.as_array() {
                Some(type_id) => {
                    let (_elem, len) = self.body_type_pool().array_def(type_id);
                    len
                }
                None => {
                    // This shouldn't happen if try_trace_place worked correctly
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: parent_type.safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        span,
                    ));
                }
            };

            // Check for constant out-of-bounds index. Evaluate at the index's
            // resolved operand types so an expression that overflows its own
            // integer type (`arr[X + 1]`, X: i8 = 127) is a compile-time error
            // rather than a folded-then-bounds-checked value (RUE-234).
            if let Some(const_idx) = self.try_get_const_index_checked(index, ctx)? {
                if const_idx < 0 || const_idx >= array_len as i128 {
                    return Err(CompileError::new(
                        ErrorKind::IndexOutOfBounds {
                            index: const_idx,
                            length: array_len,
                        },
                        self.body_rir_ref().get(index).span,
                    ));
                }
            }

            // A use as a by-ref call argument (`f(borrow a[i])`, `f(inout
            // a[i])`) borrows the element in place rather than moving it out
            // of the array (RUE-143), so the non-Copy rejection below does
            // not apply — but indexing an already-moved array is still a
            // use-after-move (`field_path()` names this element for a constant
            // index, so this catches the root's full move or this exact
            // element; the per-element check below also covers a moved-out
            // element, RUE-186).
            let is_byref_arg_use = ctx.byref_arg_root == Some(trace.root_var);
            let mut element_move: Option<i64> = None;
            if is_byref_arg_use {
                let field_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                    if let Some(moved_span) = state.is_path_moved(&field_path) {
                        return Err(super::use_after_move_path_error(
                            self.body_interner(),
                            trace.root_var,
                            &field_path,
                            span,
                            moved_span,
                        ));
                    }
                }
                self.check_read_through_moved_element(&trace, ctx, span)?;
            } else if !self.is_type_copy(elem_type) {
                self.reject_accessor_place_move(&trace, elem_type, span)?;
                // A CONSTANT index directly into an array variable moves
                // just that element out (per-element tracking, RUE-186,
                // spec 3.8:68). Everything else — dynamic index, or an
                // array that is not the trace root — keeps the rejection:
                // with a runtime index sema cannot know which element
                // moved, so neither use-after-move checking nor drop
                // suppression could stay sound (spec 7.1:28).
                element_move = self.record_element_move_out(&trace, ctx, span)?;
                if element_move.is_none() {
                    return Err(CompileError::new(
                        ErrorKind::MoveOutOfIndex {
                            element_type: elem_type
                                .safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        span,
                    )
                    .with_help(
                        "moving an element out requires a compile-time \
                         constant index into an array variable",
                    ));
                }
            }

            // Emit PlaceRead instruction
            let place_ref = Self::build_place_ref(air, &trace)?;
            if trace.via_accessor {
                ctx.accessor_place_refs.insert(
                    inst_ref,
                    (
                        place_ref,
                        trace.root_var,
                        elem_type,
                        trace.is_root_mutable,
                        trace.continues,
                    ),
                );
            }
            let mut air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: elem_type,
                span,
            });
            if let Some(k) = element_move {
                // Export the element move to drop elaboration (RUE-186).
                air_ref = self.emit_element_move_marker(
                    air,
                    &trace,
                    ctx,
                    air_ref,
                    k,
                    elem_type,
                    parent_type,
                    span,
                )?;
            }
            // Accessor guard statements (ADR-0062) run before the read.
            let air_ref = Self::finish_traced_value(air, &mut trace, air_ref, elem_type, span)?;
            return Ok(AnalysisResult::with_continues(
                air_ref,
                elem_type,
                trace.continues,
            ));
        }

        // Fallback: base is not an array place (e.g. function-call result, or
        // a String — which is a builtin struct, so `try_trace_place` bails at
        // its non-array projection). Snapshot the base root's move state
        // *before* analyzing it, in case this turns out to be a borrowing
        // String index (see below).
        let base_root = self.extract_root_variable(base);
        let base_move_state_before = base_root.and_then(|v| ctx.moved_vars.get(&v).cloned());

        // A byte/element index of StrBuf, str/Str(N), or a slice reads through
        // its base place; it does not consume that place. Classify a statically
        // typed place base before analyzing it so a StrBuf field rooted at a
        // `borrow`/`inout` parameter is read as a borrow instead of failing the
        // move check with E0429/E0437 before the type-specific path below can
        // restore its move state (RUE-725). Non-place expressions still use
        // normal value semantics and may be consumed into a temporary.
        let borrowing_base_root = base_root.filter(|_| {
            self.peek_place_type(base, ctx).is_some_and(|ty| {
                self.is_strbuf(ty) || self.is_str_like(ty) || self.slice_element_type(ty).is_some()
            })
        });
        let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, borrowing_base_root);
        let base_result = self.analyze_inst(air, base, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let base_result = base_result?;
        let base_type = base_result.ty;

        // String byte indexing: `s[i]` reads the i-th BYTE of a String as `u8`
        // (RUE-17 Phase 2, ADR-0035). O(1), bounds-checked at runtime: an
        // `index >= s.len()` traps (exit 101), exactly like array indexing.
        if self.is_strbuf(base_type) {
            return self.analyze_string_index_get(
                air,
                base_result,
                base_root,
                base_move_state_before,
                index,
                span,
                ctx,
            );
        }

        // `str` byte indexing: `s[i]` reads the i-th BYTE of a `str` as `u8`
        // (ADR-0043 Phase 3, RUE-324). A `str` is `[u8]` + UTF-8, but its bytes
        // are PACKED (1 byte each in `.rodata`), unlike an array slice whose
        // elements are 8-byte-slotted. So it cannot reuse the slice
        // `@ptr_offset`/`@ptr_read` path (which strides by `slot_count * 8`);
        // it lowers to the same packed-byte runtime read as `String`, but with
        // the 2-word `{ptr, len}` receiver: `__rue_str_byte_at(ptr, len, index)`.
        if self.is_str_like(base_type) {
            return self.analyze_str_index_get(
                air,
                base_result,
                base_root,
                base_move_state_before,
                index,
                span,
                ctx,
            );
        }

        // Slice read-indexing `s[i]` (ADR-0043, RUE-322): the base is the
        // synthetic 2-word fat-pointer struct `{ptr, len}`. Lower to a
        // runtime-bounds-checked pointer load through the fat pointer's `ptr`
        // word — no array place is involved.
        if let Some(elem_ty) = self.slice_element_type(base_type) {
            return self.analyze_slice_index_get(
                air,
                base_result,
                base_root,
                base_move_state_before,
                index,
                elem_ty,
                span,
                ctx,
            );
        }

        let index_result = self.analyze_inst(air, index, ctx)?;

        // Verify base is an array
        let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
            Some(type_id) => {
                let (element_type, length) = self.body_type_pool().array_def(type_id);
                (type_id, element_type, length)
            }
            None => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: base_type.safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    span,
                ));
            }
        };

        // Type inference normally enforces this constraint, but generic bodies
        // can reach authoritative semantic analysis before a comptime type
        // parameter has been substituted. Reject that value here as well so a
        // `type`-typed AIR operand cannot reach an index projection.
        let index_type = air.get(index_result.air_ref).ty;
        if !index_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: index_type.safe_name_with_pool(Some(self.body_type_pool())),
                },
                self.body_rir_ref().get(index).span,
            ));
        }

        // Check for constant out-of-bounds index (at the index's resolved
        // operand types, so an overflowing index expression is a compile-time
        // error rather than a folded runtime panic — RUE-234).
        if let Some(const_idx) = self.try_get_const_index_checked(index, ctx)? {
            if const_idx < 0 || const_idx >= array_len as i128 {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_idx,
                        length: array_len,
                    },
                    self.body_rir_ref().get(index).span,
                ));
            }
        }

        // Prevent moving non-Copy elements out of arrays.
        if !self.is_type_copy(elem_type) {
            return Err(CompileError::new(
                ErrorKind::MoveOutOfIndex {
                    element_type: elem_type.safe_name_with_pool(Some(self.body_type_pool())),
                },
                span,
            )
            .with_help("use explicit methods like swap() or take() to remove elements"));
        }

        // Allocate a temporary slot for the computed array value
        let num_slots = self.require_layout_slots(base_type, span)?;
        let temp_slot = self.reserve_frame_slots(&mut ctx.next_slot, num_slots, span)?;

        // Emit StorageLive for the temporary
        let storage_live_ref = air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot: temp_slot },
            ty: base_type,
            span,
        });

        // Emit Alloc to store the computed array
        let alloc_ref = air.add_inst(AirInst {
            data: AirInstData::Alloc {
                slot: temp_slot,
                init: base_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });

        // Create PlaceRead with Index projection on the temp slot
        let place_ref = air.make_place(
            AirPlaceBase::Local(temp_slot),
            base_type,
            std::iter::once(AirProjection::Index {
                array_type: base_type,
                index: index_result.air_ref,
            }),
        )?;
        let read_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place: place_ref },
            ty: elem_type,
            span,
        });

        // Note: We don't emit StorageDead here. The temporary will be cleaned up by
        // scope-based drop elaboration in the CFG builder.
        let block_ref = air.add_block(&[storage_live_ref, alloc_ref], read_ref, elem_type, span)?;
        Ok(AnalysisResult::with_continues(
            block_ref,
            elem_type,
            base_result.continues && index_result.continues,
        ))
    }

    /// Analyze a String byte index read: `s[i] -> u8` (RUE-17 Phase 2,
    /// ADR-0035).
    ///
    /// Indexing a StrBuf yields the i-th BYTE (not a char) as `u8`, lowering to
    /// the canonical source algorithm. An `index >= len` traps (exit 101),
    /// mirroring array indexing rather than producing UB.
    ///
    /// The index only *borrows* the String (it is neither consumed nor
    /// mutated), so — like a `ByRef` builtin method — we undo the move the base
    /// analysis recorded (`base_result` already analyzed) by restoring the
    /// pre-analysis move state and cancelling the emitted move marker. That
    /// keeps later uses of `s` valid and ensures the String is dropped exactly
    /// once.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn analyze_string_index_get(
        &mut self,
        air: &mut Air,
        base_result: AnalysisResult,
        base_root: Option<Spur>,
        base_move_state_before: Option<VariableMoveState>,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Un-move the borrowed receiver (mirrors ByRef builtin methods).
        if let Some(var) = base_root {
            match base_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(base_result.air_ref);

        // The index is an ordinary rvalue. Analyze it and require an integer
        // type (signed or unsigned), matching array indexing (spec 7.1:7).
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "an integer".to_string(),
                    found: index_result
                        .ty
                        .safe_name_with_pool(Some(self.body_type_pool())),
                },
                self.body_rir_ref().get(index).span,
            ));
        }

        let source_method = base_result.ty.as_struct().and_then(|struct_id| {
            (self.body_type_pool().struct_lang_item(struct_id) == Some(crate::LangItem::StrBuf))
                .then_some(struct_id)
        });
        let Some(struct_id) = source_method else {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "string indexing reached a non-canonical StrBuf".to_string(),
                ),
                span,
            ));
        };
        let method = self.body_interner().get_or_intern("byte_at_borrowed");
        if self
            .call_facts()
            .call_method_info(struct_id, method)
            .is_none()
        {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "trusted std StrBuf is missing its source `byte_at_borrowed` method"
                        .to_string(),
                ),
                span,
            ));
        }
        ctx.referenced_methods.insert((struct_id, method));
        self.record_body_method_dependency((struct_id, method));
        let (receiver, temp_scope) =
            self.materialize_borrow_argument(air, base_result.air_ref, base_result.ty, span, ctx)?;
        let call_name = self.body_interner().get_or_intern(&self.method_symbol(
            struct_id,
            "byte_at_borrowed",
            false,
        ));
        let receiver_mode = AirArgMode::Borrow;
        let call_ref = air.add_call(
            None,
            call_name,
            &[
                AirCallArg {
                    value: receiver,
                    mode: receiver_mode,
                },
                AirCallArg {
                    value: index_result.air_ref,
                    mode: AirArgMode::Normal,
                },
            ],
            Type::U8,
            span,
        )?;
        let call_ref =
            self.wrap_value_with_temp_scope(air, call_ref, Type::U8, span, temp_scope)?;
        Ok(AnalysisResult::with_continues(
            call_ref,
            Type::U8,
            base_result.continues && index_result.continues,
        ))
    }

    /// Analyze a `str` byte index read: `s[i] -> u8` (ADR-0043 Phase 3,
    /// RUE-324).
    ///
    /// A `str` is `[u8]` + UTF-8, but its bytes are PACKED (1 byte each, in
    /// `.rodata` for a literal), so indexing yields the i-th BYTE via a checked
    /// runtime call `__rue_str_byte_at(ptr, len, index)` — the same packed-byte
    /// read as `String`, minus the (nonexistent) `cap` word. The bounds check
    /// lives in the runtime: an `index >= len` traps (exit 101), mirroring array
    /// and `String` indexing rather than producing UB.
    ///
    /// Like `String` indexing, the read only *borrows* the receiver (a `str` is
    /// `Copy` anyway), so the pre-analysis move state is restored and the move
    /// marker cancelled.
    pub(super) fn analyze_str_index_get(
        &mut self,
        air: &mut Air,
        base_result: AnalysisResult,
        base_root: Option<Spur>,
        base_move_state_before: Option<VariableMoveState>,
        index: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Un-move the borrowed receiver (mirrors the String index path).
        if let Some(var) = base_root {
            match base_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(base_result.air_ref);

        // The index is an ordinary rvalue; require an integer (spec 7.1:7).
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "an integer".to_string(),
                    found: index_result
                        .ty
                        .safe_name_with_pool(Some(self.body_type_pool())),
                },
                self.body_rir_ref().get(index).span,
            ));
        }

        // Lower through the typed runtime-call contract. The 2-word `str`
        // value is passed by value; codegen decomposes it into pointer/length
        // argument registers.
        let runtime = crate::RuntimeCallKind::StrByteAt;
        let call_name = self
            .body_interner()
            .get_or_intern(runtime.helper().symbol());
        let call_ref = air.add_call(
            Some(runtime),
            call_name,
            &[
                AirCallArg {
                    value: base_result.air_ref,
                    mode: AirArgMode::Normal,
                },
                AirCallArg {
                    value: index_result.air_ref,
                    mode: AirArgMode::Normal,
                },
            ],
            Type::U8,
            span,
        )?;
        Ok(AnalysisResult::with_continues(
            call_ref,
            Type::U8,
            base_result.continues && index_result.continues,
        ))
    }

    /// Analyze a slice read-index `s[i]` (ADR-0043, RUE-322).
    ///
    /// The slice `base_result` is the synthetic 2-word fat-pointer struct
    /// `{ptr: ptr const T, len: u64}`. The read desugars to, in order:
    ///
    /// 1. a typed compiler bounds check — traps with exit 101 through the
    ///    dedicated bounds runtime helper (the same path as array indexing)
    ///    when the index is out of range;
    /// 2. `@ptr_read(@ptr_offset(ptr, i))` — `@ptr_offset` scales by
    ///    `size_of(T)`, so this reads the i-th element.
    ///
    /// The check uses the existing conditional trap lowering and the fat
    /// pointer flows through the same struct/field/pointer paths the manual
    /// `{ptr,len}` form already uses, so both backends share one implementation.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn analyze_slice_index_get(
        &mut self,
        air: &mut Air,
        base_result: AnalysisResult,
        base_root: Option<Spur>,
        base_move_state_before: Option<VariableMoveState>,
        index: InstRef,
        elem_ty: Type,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Indexing reads (never consumes) the slice receiver; undo any move the
        // receiver analysis recorded (mirrors the String index / ByRef paths).
        if let Some(var) = base_root {
            match base_move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(base_result.air_ref);

        let slice_struct_id = base_result
            .ty
            .as_struct()
            .expect("slice receiver is a synthetic struct");
        let ptr_ty = self.body_type_pool().struct_def(slice_struct_id).fields[0].ty;

        // The index is an ordinary rvalue; require an integer (spec 7.1:7).
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_integer() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "an integer".to_string(),
                    found: index_result
                        .ty
                        .safe_name_with_pool(Some(self.body_type_pool())),
                },
                self.body_rir_ref().get(index).span,
            ));
        }

        // Read the fat pointer's two words from the slice value.
        let ptr_ref = self.emit_projected_rvalue_read(
            air,
            base_result.air_ref,
            base_result.ty,
            AirProjection::Field {
                struct_id: slice_struct_id,
                field_index: 0,
            },
            ptr_ty,
            span,
            ctx,
        )?;
        let len_ref = self.emit_projected_rvalue_read(
            air,
            base_result.air_ref,
            base_result.ty,
            AirProjection::Field {
                struct_id: slice_struct_id,
                field_index: 1,
            },
            Type::U64,
            span,
            ctx,
        )?;

        // Runtime bounds check: the condition is kept in AIR as a side-effect
        // intrinsic, but its typed runtime identity is BoundsCheck rather than
        // source-level @assert. Codegen lowers that identity through the shared
        // conditional-trap machinery to the same dedicated bounds helper used
        // by fixed-array indexing. Unsigned indices only need the upper-bound
        // check; signed indices also need the lower-bound check so `s[-1]`
        // cannot read before the backing array.
        let lower_check_ref = if index_result.ty.is_signed() {
            let zero_ref = air.add_inst(AirInst {
                data: AirInstData::Const(0),
                ty: index_result.ty,
                span,
            });
            let lower_bound_ref = air.add_inst(AirInst {
                data: AirInstData::Ge(index_result.air_ref, zero_ref),
                ty: Type::BOOL,
                span,
            });
            Some(air.add_intrinsic(
                Some(crate::RuntimeCallKind::BoundsCheck),
                self.known_symbols().assert,
                &[lower_bound_ref],
                Type::UNIT,
                span,
            )?)
        } else {
            None
        };
        let upper_bound_ref = air.add_inst(AirInst {
            data: AirInstData::Lt(index_result.air_ref, len_ref),
            ty: Type::BOOL,
            span,
        });
        let upper_check_ref = air.add_intrinsic(
            Some(crate::RuntimeCallKind::BoundsCheck),
            self.known_symbols().assert,
            &[upper_bound_ref],
            Type::UNIT,
            span,
        )?;

        // element = @ptr_read(@ptr_offset(ptr, index)).
        let off_ref = air.add_intrinsic(
            None,
            self.known_symbols().ptr_offset,
            &[ptr_ref, index_result.air_ref],
            ptr_ty,
            span,
        )?;
        let elem_ref = air.add_intrinsic(
            None,
            self.known_symbols().ptr_read,
            &[off_ref],
            elem_ty,
            span,
        )?;

        // Demand-driven lowering only pulls the returned value's dependencies,
        // so the bounds-check assertion (a pure side effect) must be an explicit
        // statement of the block that yields the element.
        let stmt_refs: Vec<AirRef> = lower_check_ref
            .into_iter()
            .chain(std::iter::once(upper_check_ref))
            .collect();
        let block_ref = air.add_block(&stmt_refs, elem_ref, elem_ty, span)?;
        Ok(AnalysisResult::with_continues(
            block_ref,
            elem_ty,
            base_result.continues && index_result.continues,
        ))
    }

    /// Analyze a slice method call (ADR-0043, RUE-322). Only `.len()` is
    /// supported today: it reads the fat pointer's `len` word (runtime length).
    pub(super) fn analyze_slice_method(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        receiver_var: Option<Spur>,
        method_name: &str,
        arg_count: usize,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // The receiver is read, not consumed: analyze it as a borrow and undo
        // the move the analysis records.
        let move_state_before = receiver_var.and_then(|v| ctx.moved_vars.get(&v).cloned());
        let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, receiver_var);
        let recv_result = self.analyze_inst(air, receiver, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let recv_result = recv_result?;
        if let Some(var) = receiver_var {
            match move_state_before {
                Some(state) => {
                    ctx.moved_vars.insert(var, state);
                }
                None => {
                    ctx.moved_vars.remove(&var);
                }
            }
        }
        air.cancel_move_marker(recv_result.air_ref);

        let slice_struct_id = recv_result
            .ty
            .as_struct()
            .expect("slice receiver is a synthetic struct");

        if method_name == "len" && arg_count == 0 {
            // Read the `len` word (field 1) from the fat pointer.
            let len_ref = self.emit_projected_rvalue_read(
                air,
                recv_result.air_ref,
                recv_result.ty,
                AirProjection::Field {
                    struct_id: slice_struct_id,
                    field_index: 1,
                },
                Type::U64,
                span,
                ctx,
            )?;
            return Ok(AnalysisResult::new(len_ref, Type::U64));
        }

        Err(CompileError::new(
            ErrorKind::UndefinedMethod {
                method_name: method_name.to_string(),
                type_name: self
                    .body_type_pool()
                    .struct_def(slice_struct_id)
                    .name
                    .to_string(),
            },
            span,
        ))
    }

    /// Analyze a field assignment through the shared place representation.
    pub(crate) fn analyze_place_set(
        &mut self,
        air: &mut Air,
        place: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Assignment evaluates the RHS before the target (spec 5.2:17-18).
        // Peek only at the target type for contextual RHS analysis; tracing
        // an accessor target is deferred until after the RHS so its loan
        // cannot reject an otherwise valid RHS use of the receiver.
        let cached_before_rhs = ctx.accessor_place_refs.get(&place).copied();
        let destination_type = cached_before_rhs.map_or_else(
            || {
                self.peek_place_type(place, ctx)
                    .ok_or_else(|| CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
            },
            |(_, _, ty, _, _)| Ok(ty),
        )?;
        let previous_expected = ctx.expected_type.replace(destination_type);
        let rhs_shared_reads_before = ctx.expression_shared_reads.len();
        let rhs_exclusive_uses_before = ctx.expression_exclusive_uses.len();
        let value_result = self.analyze_inst(air, value, ctx);
        ctx.expected_type = previous_expected;
        let value_result = value_result?;
        self.reject_accessor_result_escape(value, AccessorEscapeSite::Store, span, ctx)?;
        let reachable_edges_after_rhs = ctx.loop_break_stack.clone();
        // A plain assignment's RHS is complete before the LHS place is
        // evaluated. Ordinary shared-read markers are expression-order
        // bookkeeping and can be truncated before the target is traced;
        // accessor loans, however, span the entire assignment expression and
        // must remain live so two accessor results on one root conflict.
        // Compound assignment analyzes the same target instruction as the
        // RHS's read (the compound value is `target op rhs`). That read may
        // populate the accessor-place cache only while analyzing the RHS, so
        // refresh it before deciding whether the target has already been
        // evaluated. A plain assignment's separately generated RHS cannot
        // populate the target's instruction entry.
        let cached = cached_before_rhs.or_else(|| ctx.accessor_place_refs.get(&place).copied());
        if cached.is_none() {
            ctx.expression_shared_reads
                .truncate(rhs_shared_reads_before);
            ctx.expression_exclusive_uses
                .truncate(rhs_exclusive_uses_before);
        }
        let (place_ref, root_var, mutable, place_continues) =
            if let Some((place_ref, root, _, mutable, continues)) = cached {
                (place_ref, root, mutable, continues)
            } else {
                let trace = self
                    .try_trace_place(place, air, ctx)?
                    .ok_or_else(|| CompileError::new(ErrorKind::InvalidAssignmentTarget, span))?;
                if !value_result.continues {
                    Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_rhs);
                }
                if !trace.via_accessor || !trace.is_root_mutable {
                    return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
                }
                let root = trace.root_var;
                self.reject_mutate_iter_borrowed(root, span, ctx)?;
                (
                    Self::build_place_ref(air, &trace)?,
                    root,
                    trace.is_root_mutable,
                    trace.continues,
                )
            };
        if !mutable {
            return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
        }
        if cached.is_some() {
            self.reject_mutate_iter_borrowed(root_var, span, ctx)?;
        }
        self.check_linear_overwrite(destination_type, false, false, span)?;
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::PlaceWrite {
                place: place_ref,
                value: value_result.air_ref,
            },
            ty: Type::UNIT,
            span,
        });
        let continues = value_result.continues && place_continues;
        if continues {
            self.record_completed_exclusive_use(root_var, span, ctx);
        }
        Ok(AnalysisResult::with_continues(
            air_ref,
            Type::UNIT,
            continues,
        ))
    }

    /// Analyze a field assignment through the shared place representation.
    pub(crate) fn analyze_field_set(
        &mut self,
        air: &mut Air,
        base: InstRef,
        field: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Assignment evaluates the RHS before the target (spec 5.2:17-18).
        // Analyze the value first, then keep any target edges from a dead RHS
        // out of the enclosing loop's reachable control-flow facts.
        let value_result = self.analyze_inst(air, value, ctx)?;
        self.reject_accessor_result_escape(value, AccessorEscapeSite::Store, span, ctx)?;
        let reachable_edges_after_value = ctx.loop_break_stack.clone();
        let traced = self.try_trace_place(base, air, ctx)?;
        if !value_result.continues {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_value);
        }

        if let Some(mut trace) = traced {
            // Check if the root variable was fully moved
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.body_interner().resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(root_name)));
                }
            }

            // Writing through a field of a collection an enclosing `for` loop
            // is iterating mutates a shared-borrowed value (spec 4.8:26,
            // RUE-233) — E0428, like an explicit `borrow` parameter.
            self.reject_mutate_iter_borrowed(trace.root_var, span, ctx)?;

            // A write must not overlap an incompatible accessor-result loan
            // on the same root (ADR-0062, E0259). A write through this trace's
            // own exclusive accessor place is the operation that loan permits.
            if !trace.is_root_mutable || !trace.via_accessor {
                self.reject_accessor_loan_conflict(trace.root_var, "by assignment", span, ctx)?;
            }

            // Check mutability
            let root_name = self.body_interner().resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                // Provide more specific error based on whether it's a param or local
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(self.immutable_param_assign_error(&root_name, root_type, span));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                    AirPlaceBase::Accessor(_) | AirPlaceBase::Indirect(_) => {
                        unreachable!("accessor places are classified as borrowed")
                    }
                }
            }

            // Add the final field projection
            let base_type = trace.result_type();
            let struct_id = match base_type.as_struct() {
                Some(id) => id,
                None => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        span,
                    ));
                }
            };

            let struct_def = self.body_type_pool().struct_def(struct_id);
            let field_name = self.body_interner().resolve(&field);
            let Some((field_index, struct_field)) = struct_def.find_field(field_name) else {
                return Err(CompileError::new(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.to_string(),
                        field_name: field_name.to_string(),
                    },
                    span,
                ));
            };

            let field_type = struct_field.ty;

            // Add the field projection to the trace
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Field {
                    struct_id,
                    field_index: field_index as u32,
                },
                result_type: field_type,
                field_name: Some(field),
                const_index: None,
                index_segment: None,
            });

            // A write through an element of a partially moved array
            // (`xs[0].f = ...` after an element of `xs` moved out) is
            // rejected (RUE-186, E0480), like a direct element write.
            self.reject_write_into_partially_moved_array(&trace, ctx, span, None)?;

            // RUE-387: writing a live linear value's field would silently drop
            // the old field value. Legal only when that exact field path was
            // proven moved-out on every path (`consume(o.f); o.f = ...`); a
            // dynamic index anywhere in the chain can never prove it.
            let discharged = !trace.has_untrackable_index()
                && self.place_linear_discharged(
                    field_type,
                    trace.root_var,
                    &trace.field_path(),
                    ctx,
                );
            self.check_linear_overwrite(field_type, discharged, false, span)?;

            // The write reinitializes its destination: the assigned path
            // (and any moved sub-paths under it) is no longer moved, so
            // `o.f = ...` after `consume(o.f)` makes `o.f` usable again.
            // Writes through an index projection are skipped: a partially
            // moved array is already rejected outright above (E0480), and
            // per-element write re-arm is unsupported (the sema path and the
            // runtime drop flags would disagree), so the whole array must be
            // reinitialized instead — never a single `arr[0].f` write.
            if !trace
                .projections
                .iter()
                .any(|p| matches!(p.proj, AirProjection::Index { .. }))
            {
                let assigned_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get_mut(&trace.root_var) {
                    state.mark_path_reinitialized(&assigned_path);
                    if state.is_empty() {
                        ctx.moved_vars.remove(&trace.root_var);
                    }
                }
            }

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace)?;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            let continues = trace.continues && value_result.continues;
            if continues {
                self.record_completed_exclusive_use(trace.root_var, span, ctx);
            }
            return Ok(AnalysisResult::with_continues(
                air_ref,
                Type::UNIT,
                continues,
            ));
        }

        // Fallback: base is not a place (e.g., function call result)
        // This shouldn't normally happen for valid assignment targets
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
    }

    /// Implementation for IndexSet - handles both local and parameter array index assignment.
    /// Analyze an index assignment through the shared place representation.
    pub(crate) fn analyze_index_set(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Keep the one shape that can reinitialize itself: a direct constant
        // element read from the same root array.  The RHS is analyzed first,
        // so its element move is present when the target-side partial-array
        // guard runs; remember that this exact path was new before the RHS.
        let rhs_element = self.direct_constant_array_element_path(value);
        let rhs_element_was_unmoved = rhs_element.as_ref().is_some_and(|(root, path)| {
            ctx.moved_vars
                .get(root)
                .is_none_or(|state| state.is_path_or_descendant_moved(path).is_none())
        });
        // Assignment evaluates the RHS before the target and index
        // (spec 5.2:17-18). Analyze the value first so dead target edges do
        // not become reachable loop back edges.
        let value_result = self.analyze_inst(air, value, ctx)?;
        self.reject_accessor_result_escape(value, AccessorEscapeSite::Store, span, ctx)?;
        let reachable_edges_after_rhs = ctx.loop_break_stack.clone();
        let traced = self.try_trace_place(base, air, ctx)?;
        if !value_result.continues {
            Self::restore_reachable_loop_edges(ctx, &reachable_edges_after_rhs);
        }

        if let Some(mut trace) = traced {
            // Check if the root variable was fully moved
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.body_interner().resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span)
                    .with_help(super::borrow_instead_of_move_help(root_name)));
                }
            }

            // Writing an element of a collection an enclosing `for` loop is
            // iterating mutates a shared-borrowed value (spec 4.8:26,
            // RUE-233) — E0428, like an explicit `borrow` parameter.
            self.reject_mutate_iter_borrowed(trace.root_var, span, ctx)?;

            // A write must not overlap an incompatible accessor-result loan
            // on the same root (ADR-0062, E0259). A write through this trace's
            // own exclusive accessor place is the operation that loan permits.
            if !trace.is_root_mutable || !trace.via_accessor {
                self.reject_accessor_loan_conflict(trace.root_var, "by assignment", span, ctx)?;
            }

            // Check mutability
            let root_name = self.body_interner().resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(self.immutable_param_assign_error(&root_name, root_type, span));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                    AirPlaceBase::Accessor(_) | AirPlaceBase::Indirect(_) => {
                        unreachable!("accessor places are classified as borrowed")
                    }
                }
            }

            // Get array type info from the trace
            let base_type = trace.result_type();
            let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
                Some(id) => {
                    let (elem, len) = self.body_type_pool().array_def(id);
                    (id, elem, len)
                }
                None => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.safe_name_with_pool(Some(self.body_type_pool())),
                        },
                        span,
                    ));
                }
            };

            // Analyze index. Index must be an integer type (signed or
            // unsigned) per spec 7.1:7; negative/out-of-range runtime indices
            // trap at runtime via the bounds check (RUE-81).
            let reachable_edges_before_index = ctx.loop_break_stack.clone();
            let index_result = self.analyze_inst(air, index, ctx)?;
            if !(value_result.continues && trace.continues) {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_index);
            }
            if !index_result.ty.is_integer() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer type".to_string(),
                        found: index_result
                            .ty
                            .safe_name_with_pool(Some(self.body_type_pool())),
                    },
                    self.body_rir_ref().get(index).span,
                ));
            }

            // Compile-time bounds check for constant indices, evaluated at the
            // index's resolved operand types so an overflowing index expression
            // is a compile-time error, not a folded runtime panic (RUE-234).
            if let Some(const_index) = self.try_get_const_index_checked(index, ctx)? {
                if const_index < 0 || const_index >= array_len as i128 {
                    return Err(CompileError::new(
                        ErrorKind::IndexOutOfBounds {
                            index: const_index,
                            length: array_len,
                        },
                        self.body_rir_ref().get(index).span,
                    ));
                }
            }

            // Add the index projection. A non-negative constant index carries
            // its element path segment so field_path nests through it (RUE-279).
            let const_index = self.try_get_const_index(index);
            let index_segment = match const_index {
                Some(k) if k >= 0 => Some(index_path_segment(self.body_interner(), k as u64)),
                _ => None,
            };
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Index {
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                result_type: elem_type,
                field_name: None,
                const_index,
                index_segment,
            });

            // Writing into an array with moved-out elements is rejected
            // (RUE-186, E0480): the write can't re-arm per-element ownership.
            let reinitialized_path = rhs_element
                .filter(|(root, path)| {
                    rhs_element_was_unmoved
                        && *root == trace.root_var
                        && path == &trace.field_path()
                        && ctx.moved_vars.get(root).is_some_and(|state| {
                            state.partial_moves.iter().any(|(moved, _)| moved == path)
                        })
                })
                .map(|(_, path)| path);
            self.reject_write_into_partially_moved_array(
                &trace,
                ctx,
                span,
                reinitialized_path.as_deref(),
            )?;

            // RUE-387: writing a live linear value into an array element would
            // silently drop the old element. Legal only when that exact
            // constant-index element was proven moved-out on every path
            // (`arr[0] = f(arr[0])`); a runtime index can never prove which
            // element is live, so it is always rejected (repro 3).
            let discharged = !trace.has_untrackable_index()
                && self.place_linear_discharged(
                    elem_type,
                    trace.root_var,
                    &trace.field_path(),
                    ctx,
                );
            self.check_linear_overwrite(elem_type, discharged, false, span)?;

            // The write reinitializes its destination element: the assigned
            // path is no longer moved, so `arr[0] = arr[0]` un-marks the
            // move-out the RHS `arr[0]` just recorded and leaves the element
            // usable again (spec 3.8:55). This mirrors `analyze_field_set`,
            // which reinitializes an assigned field path. Only a direct,
            // constant-index element of a root array is reinitialized: that is
            // exactly the shape `record_element_move_out` tracks per element
            // (`projections == [Index]` with a known index), so a nested or
            // dynamic index — which was never recorded as a per-element move —
            // is conservatively left alone (RUE-228).
            if let [
                ProjectionInfo {
                    const_index: Some(k),
                    ..
                },
            ] = trace.projections.as_slice()
            {
                if *k >= 0 {
                    let elem_path = vec![index_path_segment(self.body_interner(), *k as u64)];
                    if let Some(state) = ctx.moved_vars.get_mut(&trace.root_var) {
                        state.mark_path_reinitialized(&elem_path);
                        if state.is_empty() {
                            ctx.moved_vars.remove(&trace.root_var);
                        }
                    }
                }
            }

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace)?;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            let continues = trace.continues && index_result.continues && value_result.continues;
            if continues {
                self.record_completed_exclusive_use(trace.root_var, span, ctx);
            }
            return Ok(AnalysisResult::with_continues(
                air_ref,
                Type::UNIT,
                continues,
            ));
        }

        // Fallback: base is not a place
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
    }

    /// Check if directives contain @allow for a specific warning name.
    pub(crate) fn has_allow_directive<'r>(
        &self,
        directives: impl Iterator<Item = rue_rir::RirDirectiveView<'r>>,
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.body_interner().get("allow");
        let warning_sym = self.body_interner().get(warning_name);

        for directive in directives {
            if Some(directive.name) == allow_sym {
                for arg in &directive.args {
                    if Some(*arg) == warning_sym {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check for unused local variables in the current scope (before popping it).
    /// Uses the scope stack to determine which variables were added in the current scope.
    pub(crate) fn check_unused_locals_in_current_scope(&self, ctx: &mut AnalysisContext) {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return;
        };

        for (symbol, _old_value) in current_scope {
            // A function-level `@allow(unused_variable)` suppresses unused
            // variable warnings for all bindings in the function body.
            if ctx.allow_unused_variables {
                continue;
            }

            // Skip if variable was used
            if ctx.used_locals.contains(symbol) {
                continue;
            }

            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Get variable name
            let name = self.body_interner().resolve(&*symbol);

            // Skip variables starting with underscore (convention for intentionally unused)
            if name.starts_with('_') {
                continue;
            }

            // Skip if @allow(unused_variable) was applied
            if local.allow_unused {
                continue;
            }

            // Emit warning with help suggestion into this function's warning buffer.
            ctx.warnings.push(
                CompileWarning::new(WarningKind::UnusedVariable(name.to_string()), local.span)
                    .with_help(format!(
                        "if this is intentional, prefix it with an underscore: `_{}`",
                        name
                    )),
            );
        }
    }

    /// Check for unconsumed linear values in the current scope (before popping it).
    /// Linear values MUST be consumed (moved) - it's an error to let them drop implicitly.
    /// Returns an error if any linear value was not consumed.
    pub(crate) fn check_unconsumed_linear_values(
        &self,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return Ok(());
        };

        for (symbol, _old_value) in current_scope {
            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };
            let state = ctx.moved_vars.get(symbol);
            self.check_linear_binding_consumed(*symbol, local, state)?;
        }

        Ok(())
    }

    /// The must-consume obligation for one binding against one move state —
    /// the shared kernel of the scope-exit walk above and the early-exit edge
    /// walk below ([`Self::check_linear_values_at_exit_edge`]).
    ///
    /// Consumption must hold on EVERY path (must-consume): `full_move` alone
    /// is may-move (union at branch joins), so a value consumed in only one
    /// branch of an if/match still leaks on the other paths. Only types that
    /// require consumption are checked: linear structs themselves, and
    /// non-empty arrays whose elements carry one (an array of linear values
    /// must be consumed — as a whole, element-wise via constant-index moves
    /// (RUE-186), or per element through its linear sub-places (RUE-1606);
    /// dropping it would silently drop every element).
    fn check_linear_binding_consumed(
        &self,
        symbol: Spur,
        local: &LocalVar,
        state: Option<&VariableMoveState>,
    ) -> CompileResult<()> {
        if !self.type_requires_consumption(local.ty) {
            return Ok(());
        }
        if state.is_some_and(|s| s.full_move_on_all_paths) {
            return Ok(());
        }

        // Per-place residue (core §5.6, spec 3.8:60, RUE-1591): consuming
        // exactly the linear sub-places of an infectious carrier
        // discharges its obligation, and the non-linear residue is left
        // for the ordinary scope-exit drop walk. The walk recurses into
        // array elements (RUE-1606), so it also accepts complete
        // element-wise consumption of a root linear array (RUE-186, spec
        // 3.8:71) and of an array field consumed through per-element field
        // moves.
        let Some(residue) = self.residual_linear_place(local.ty, state, &mut Vec::new()) else {
            return Ok(());
        };

        // A root array PARTIALLY consumed element-wise gets the more
        // precise diagnostic naming the missing elements (RUE-186).
        match self.check_array_elementwise_consumption(local.ty, state, symbol, local.span)? {
            ElementwiseConsumption::Complete => return Ok(()),
            ElementwiseConsumption::NotElementwise => {}
        }

        let name = self.body_interner().resolve(&symbol);
        let err = linear_not_consumed_error(
            name,
            local.span,
            Self::residue_consumed_on_some_path(state, &residue),
        );
        let err = self.note_residual_linear_place(err, symbol, &residue);
        Err(self.attach_infectious_linear_note(err, local.ty))
    }

    /// The must-consume obligation for one by-value parameter against one
    /// move state (spec 3.8:62, RUE-61): the callee owns its pass-by-value
    /// parameters and drops them at exit unless moved out, so a by-value
    /// parameter carrying a linear value must be consumed on every path —
    /// exactly like a linear local. `inout`/`borrow` parameters stay owned by
    /// the caller and comptime parameters are substituted away, so both are
    /// exempt here; the destructor exemption for `self` (3.8:62) is the
    /// caller's to apply, since it is a property of the body, not of the
    /// parameter.
    ///
    /// Shared by the end-of-body check in `analyze_function_internal` (the
    /// fall-through exit) and the early-exit edge walk below (RUE-1614), so
    /// both exits enforce one rule with one diagnostic shape.
    pub(crate) fn check_linear_param_consumed(
        &self,
        param: &ParamInfo,
        state: Option<&VariableMoveState>,
        span: Span,
    ) -> CompileResult<()> {
        if param.mode != RirParamMode::Normal || param.is_comptime {
            return Ok(());
        }
        if !self.type_requires_consumption(param.ty) {
            return Ok(());
        }
        if state.is_some_and(|s| s.full_move_on_all_paths) {
            return Ok(());
        }
        // Per-place residue (core §5.6, RUE-1591): a by-value parameter of an
        // infectious carrier is consumed by consuming its linear sub-places;
        // the rest is dropped by the ordinary exit walk, exactly as for a
        // local. The walk recurses into array elements (RUE-1606), so
        // element-wise consumption of a linear array parameter (RUE-186) —
        // and per-element field consumption of an array anywhere in the
        // parameter's place tree — satisfies the obligation like a whole
        // move.
        let Some(residue) = self.residual_linear_place(param.ty, state, &mut Vec::new()) else {
            return Ok(());
        };
        // A root array PARTIALLY consumed element-wise gets the more precise
        // diagnostic naming the missing elements (RUE-186).
        match self.check_array_elementwise_consumption(param.ty, state, param.name, span)? {
            ElementwiseConsumption::Complete => return Ok(()),
            ElementwiseConsumption::NotElementwise => {}
        }
        let name = self.body_interner().resolve(&param.name);
        let err = linear_not_consumed_error(
            name,
            span,
            Self::residue_consumed_on_some_path(state, &residue),
        )
        .with_note(format!(
            "parameter '{name}' is passed by value, so this function owns it \
             and must consume it (pass it on, return it, or destructure it)"
        ));
        let err = self.note_residual_linear_place(err, param.name, &residue);
        Err(self.attach_infectious_linear_note(err, param.ty))
    }

    /// RUE-1614: enforce the linear must-consume obligation at an early-exit
    /// EDGE — a `return`, the `?` operator's failure path, a `break`, or a
    /// `continue` — against the move state in force AT that edge.
    ///
    /// The scope-exit walk above runs over the state at each block's
    /// fall-through exit. After a *conditional* early exit the enclosing
    /// branch joins carry only the non-diverging continuation (diverging arms
    /// are excluded from the join), so the edge's own state is never observed
    /// by any scope-exit check — a linear value live only across the edge
    /// escaped into the exit's unwind drops and was destroyed unconsumed.
    /// (A *straight-line* early exit is caught without this walk: the
    /// enclosing block's divergence snapshot re-runs the scope-exit check on
    /// the state at the divergence.)
    ///
    /// The walk visits every binding introduced by the scope frames the edge
    /// unwinds — `ctx.scope_stack[first_unwound_frame..]` — innermost frame
    /// first, entries in declaration order within a frame (matching the
    /// scope-exit walk's report order). Because `locals`/`moved_vars` are
    /// keyed by NAME (RUE-522), a frame entry's saved shadow value is
    /// virtually restored as the walk unwinds past it, exactly as `pop_scope`
    /// would, so shadowed outer bindings are checked with their OWN state,
    /// and a comptime alias that merely hides a local restores the hidden
    /// binding for the outer frames' checks.
    ///
    /// `function_exit` additionally checks the by-value parameters (spec
    /// 3.8:62) — a `return` or `?` unwinds them too — unless the body is a
    /// destructor, whose `self` is disposed of by the drop glue (3.8:62).
    /// `break`/`continue` pass `false`: bindings outside the loop stay live
    /// and may still be consumed after it.
    pub(crate) fn check_linear_values_at_exit_edge(
        &self,
        ctx: &AnalysisContext,
        first_unwound_frame: usize,
        function_exit: bool,
    ) -> CompileResult<()> {
        // The virtually-unwound view: a symbol present here has had its
        // innermost binding(s) unwound, and maps to the shadowed binding (or
        // absence) that pop_scope would restore.
        let mut unwound_locals: AHashMap<Spur, Option<&LocalVar>> = AHashMap::new();
        let mut unwound_moves: AHashMap<Spur, Option<&VariableMoveState>> = AHashMap::new();

        for frame_idx in (first_unwound_frame..ctx.scope_stack.len()).rev() {
            let frame = &ctx.scope_stack[frame_idx];
            // Pushed pairwise with `frame` by `insert_local` and
            // `bind_comptime_type_var`, so `move_frame[k]` below is the same
            // binding as `frame[k]` (a desync would index out of bounds).
            let move_frame = &ctx.moved_scope_stack[frame_idx];

            // Resolve the binding each entry introduced (reverse order, so a
            // later same-name entry's saved shadow value feeds the earlier
            // entry), then check in declaration order.
            let mut resolved: Vec<(Spur, Option<&LocalVar>, Option<&VariableMoveState>)> =
                Vec::with_capacity(frame.len());
            for (k, (symbol, old_local)) in frame.iter().enumerate().rev() {
                let local = match unwound_locals.get(symbol) {
                    Some(saved) => *saved,
                    None => ctx.locals.get(symbol),
                };
                let state = match unwound_moves.get(symbol) {
                    Some(saved) => *saved,
                    None => ctx.moved_vars.get(symbol),
                };
                resolved.push((*symbol, local, state));
                unwound_locals.insert(*symbol, old_local.as_ref());
                unwound_moves.insert(*symbol, move_frame[k].1.as_ref());
            }
            for (symbol, local, state) in resolved.into_iter().rev() {
                // An entry can name a non-local binding (a comptime type
                // alias hiding a local pushes one); it introduces no value.
                let Some(local) = local else { continue };
                self.check_linear_binding_consumed(symbol, local, state)?;
            }
        }

        if function_exit && !ctx.is_destructor {
            // The parameter diagnostic points at the body, matching the
            // end-of-body check's shape (there is no binding span to cite).
            let body_span = self.body_rir_ref().get(ctx.producer).span;
            for param in ctx.params {
                let state = match unwound_moves.get(&param.name) {
                    Some(saved) => *saved,
                    None => ctx.moved_vars.get(&param.name),
                };
                self.check_linear_param_consumed(param, state, body_span)?;
            }
        }

        Ok(())
    }

    /// Core §5.6 `residual-linear(Σ, p, T)`: the path (relative to the root
    /// variable) of a sub-place of `p: T` that still holds an unconsumed
    /// linear value under the move state `state`, or `None` when the
    /// must-consume obligation is fully discharged.
    ///
    /// The leak check is keyed on the RESIDUAL ownership state, not on the
    /// binding's type alone (RUE-1591): after a partial move the obligation
    /// attaches to whatever linear content is still present, so consuming
    /// exactly the linear part of an infectious carrier
    /// (`let c = C { l: token, d: junk }; sink(c.l);`) is legal and the
    /// non-linear residue (`c.d`, destructor and all) is dropped by the
    /// ordinary scope-exit walk.
    ///
    /// Following the core's clauses:
    /// - a place moved out on every path holds nothing — `None`;
    /// - a **declared**-`linear` struct's obligation belongs to the value
    ///   itself, not its contents (3.8:74), so a live one is always residue.
    ///   A husk whose linear field was moved out is reached only through the
    ///   moved-out clause above, which keeps the current husk acceptance
    ///   (RUE-614, an open maintainer decision) unchanged;
    /// - a struct otherwise recurses into the fields that carry a linear
    ///   value;
    /// - an array recurses into its elements once any move was recorded
    ///   under this place (see [`Self::residual_linear_array_place`]), so
    ///   consuming the linear content of every element — element-wise for a
    ///   root array (3.8:71), or through per-element field moves for an
    ///   array anywhere in the place tree (RUE-1606) — discharges it;
    /// - everything else (enums, scalars) falls back to the type's own
    ///   obligation, because their sub-places are not tracked per place here.
    ///
    /// `path` is scratch space threaded through the recursion; it is restored
    /// to its entry value before returning.
    pub(crate) fn residual_linear_place(
        &self,
        ty: Type,
        state: Option<&VariableMoveState>,
        path: &mut Vec<Spur>,
    ) -> Option<Vec<Spur>> {
        if state.is_some_and(|s| Self::place_moved_on_all_paths(s, path)) {
            return None;
        }
        if let TypeKind::Array(array_id) = ty.kind() {
            return self.residual_linear_array_place(array_id, state, path);
        }
        let Some(struct_id) = ty.as_struct() else {
            return self.type_requires_consumption(ty).then(|| path.clone());
        };
        if self.struct_declared_linear(struct_id) {
            return Some(path.clone());
        }
        let def = self.body_type_pool().struct_def(struct_id);
        for field in def.fields.iter() {
            if !self.type_carries_linear(field.ty) {
                continue;
            }
            path.push(self.body_interner().get_or_intern(&field.name));
            let found = self.residual_linear_place(field.ty, state, path);
            path.pop();
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// The array clause of [`Self::residual_linear_place`] (RUE-1606): the
    /// residual obligation of an array-typed place, per element.
    ///
    /// An array's linear content can be consumed below the whole-array
    /// granularity: element-wise constant-index moves for a root array
    /// (3.8:68/3.8:71), and — for an array ANYWHERE in the place tree whose
    /// elements are infectious carriers — per-element field moves
    /// (`h.arr[0].p`; the field-move path model names constant-index
    /// elements, RUE-279). Recursing into each element at its own path lets
    /// both discharge the array exactly like a struct field's consumption
    /// discharges the struct (core §5.6).
    ///
    /// When NO move was recorded strictly under this place, no element
    /// content was ever consumed, so the whole array is the residue — this
    /// keeps the untouched-array diagnostic (and the walk's cost) independent
    /// of the array length. Declared-`linear` elements are never dischargeable
    /// below whole-element granularity (their obligation is the value itself,
    /// 3.8:74, and an element of a non-root array cannot be moved out at all,
    /// 3.8:68), so for them the recursion reports the first live element.
    fn residual_linear_array_place(
        &self,
        array_id: crate::types::ArrayTypeId,
        state: Option<&VariableMoveState>,
        path: &mut Vec<Spur>,
    ) -> Option<Vec<Spur>> {
        let (elem_ty, len) = self.body_type_pool().array_def(array_id);
        if len == 0 || !self.type_requires_consumption(elem_ty) {
            return None;
        }
        let touched_below = state.is_some_and(|s| {
            s.partial_moves
                .iter()
                .any(|(p, _)| p.len() > path.len() && p[..path.len()] == path[..])
        });
        if !touched_below {
            return Some(path.clone());
        }
        for k in 0..len {
            path.push(index_path_segment(self.body_interner(), k));
            let found = self.residual_linear_place(elem_ty, state, path);
            path.pop();
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// The span at which the residual linear place was consumed on SOME path,
    /// when it was — that selects the more precise "not consumed on all paths"
    /// diagnostic (E0443) over the bare "was dropped" one (E0406).
    ///
    /// A whole-value move takes precedence: it is the coarser, more likely
    /// intent. Otherwise the residue's own may-move span is what the author
    /// needs to see — the branch where they DID consume it (RUE-1591).
    pub(crate) fn residue_consumed_on_some_path(
        state: Option<&VariableMoveState>,
        residue: &[Spur],
    ) -> Option<Span> {
        let state = state?;
        state.full_move.or_else(|| {
            state
                .partial_moves
                .iter()
                .find(|(p, _)| p == residue)
                .map(|(_, span)| *span)
        })
    }

    /// Whether `path` (or an ancestor prefix of it) was moved out on EVERY
    /// non-diverging path reaching this point.
    ///
    /// Must-move, not may-move: a sub-place consumed in only one branch of an
    /// `if` is still dropped on the other paths, so it does not discharge a
    /// linear obligation. A whole-variable move subsumes every path under it
    /// (`mark_path_moved(&[])` clears the partial sets for exactly that
    /// reason).
    fn place_moved_on_all_paths(state: &VariableMoveState, path: &[Spur]) -> bool {
        if state.full_move_on_all_paths {
            return true;
        }
        (1..=path.len()).any(|len| {
            state
                .partial_moves_on_all_paths
                .contains(&path[..len].to_vec())
        })
    }

    /// Name the sub-place that still carries the unconsumed linear value, so a
    /// partially consumed carrier does not report a bare "consume the whole
    /// value" diagnostic (RUE-1591). A residue at the root itself adds
    /// nothing the message does not already say.
    pub(crate) fn note_residual_linear_place(
        &self,
        err: rue_error::CompileError,
        root_var: Spur,
        residue: &[Spur],
    ) -> rue_error::CompileError {
        if residue.is_empty() {
            return err;
        }
        let place = super::format_move_path(self.body_interner(), root_var, residue);
        err.with_note(format!(
            "'{place}' still holds a linear value; consume it (or the whole value)"
        ))
    }

    /// Does dropping a value of this type discard a linear value, i.e. does
    /// the type carry a must-consume obligation?
    ///
    /// This is [`Self::type_carries_linear`] with one refinement (RUE-194,
    /// spec 3.8:74): an array shape whose total element count is zero
    /// (`[L; 0]`, `[[L; 5]; 0]`, `[[L; 0]; 5]`, ...) holds no linear values
    /// at runtime, so its obligation is vacuously satisfied and dropping it
    /// is fine. A linear STRUCT always requires consumption — the `linear`
    /// declaration is the obligation, regardless of what its fields hold.
    pub(crate) fn type_requires_consumption(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.body_type_pool().array_def(array_id);
                length > 0 && self.type_requires_consumption(element_type)
            }
            _ => self.type_carries_linear(ty),
        }
    }

    /// Shared element-wise consumption check for linear arrays (RUE-186):
    /// returns `Complete` when every element's linear content was consumed
    /// on every path — a whole-element move (constant index, 3.8:68), or
    /// consumption of all of the element's linear sub-places (RUE-1606, the
    /// per-place residual model of core §5.6) — an `Err` naming the missing
    /// elements when the array was only PARTIALLY consumed element-wise, and
    /// `NotElementwise` when no element was ever touched (or the type is not
    /// an array) — the caller then reports its usual whole-value diagnostic.
    ///
    /// The reinit-after-overwrite proof is deliberately NOT this check: see
    /// [`Self::array_fully_moved_elementwise`].
    pub(crate) fn check_array_elementwise_consumption(
        &self,
        ty: Type,
        state: Option<&super::super::context::VariableMoveState>,
        symbol: Spur,
        decl_span: Span,
    ) -> CompileResult<ElementwiseConsumption> {
        let TypeKind::Array(array_id) = ty.kind() else {
            return Ok(ElementwiseConsumption::NotElementwise);
        };
        let Some(s) = state else {
            return Ok(ElementwiseConsumption::NotElementwise);
        };
        let (elem_ty, len) = self.body_type_pool().array_def(array_id);
        let elem_path = |k: u64| vec![index_path_segment(self.body_interner(), k)];
        let unconsumed: Vec<u64> = (0..len)
            .filter(|k| {
                self.residual_linear_place(elem_ty, Some(s), &mut elem_path(*k))
                    .is_some()
            })
            .collect();
        if unconsumed.is_empty() {
            return Ok(ElementwiseConsumption::Complete);
        }
        let touched_any_element = s.partial_moves.iter().any(|(p, _)| {
            p.first()
                .is_some_and(|seg| is_index_segment(self.body_interner(), *seg))
        });
        if !touched_any_element {
            return Ok(ElementwiseConsumption::NotElementwise);
        }
        let name = self.body_interner().resolve(&symbol);
        // An unconsumed element that WAS moved (whole, or below the element)
        // on some path selects the more precise "not consumed on all paths"
        // diagnostic (E0443 over E0406).
        let some_path_span = unconsumed.iter().find_map(|k| {
            let target = elem_path(*k);
            s.partial_moves
                .iter()
                .find(|(p, _)| p.first() == Some(&target[0]))
                .map(|(_, span)| *span)
        });
        let list = unconsumed
            .iter()
            .map(|k| format!("[{k}]"))
            .collect::<Vec<_>>()
            .join(", ");
        Err(
            linear_not_consumed_error(name, decl_span, some_path_span).with_note(format!(
                "the array is partially consumed: element(s) {list} of \
                 '{name}' are not consumed on every path"
            )),
        )
    }

    /// RUE-387: reject an assignment `p = e` that would overwrite a place
    /// still holding a live linear value.
    ///
    /// Steve's ruling (2026-07-09): assignment to an initialized place holding
    /// a linear value is a compile error unless the old value has provably been
    /// moved/consumed first — there is no implicit consume-on-overwrite. The
    /// overwrite-drop machinery (#968, spec 3.9:18) would otherwise silently
    /// drop the old linear value, a theorem-5 soundness hole (docs/formal
    /// §5.2 D-Assign).
    ///
    /// The check is TYPE-based (`type_requires_consumption`), not state-based:
    /// it fires whenever the destination's type carries a must-consume
    /// obligation. The sole carve-out is the reinit-after-move idiom (spec
    /// 3.8:55/56): a place proven moved-out on every path holds nothing to
    /// destroy. `discharged` is that proof, computed by the caller via
    /// [`Self::place_linear_discharged`]; a runtime-index element can never
    /// prove it and passes `false`.
    pub(crate) fn check_linear_overwrite(
        &self,
        dest_ty: Type,
        discharged: bool,
        through_inout: bool,
        span: Span,
    ) -> CompileResult<()> {
        if !self.type_requires_consumption(dest_ty) || discharged {
            return Ok(());
        }
        let type_name = dest_ty.safe_name_with_pool(Some(self.body_type_pool()));
        let err = if through_inout {
            CompileError::new(
                ErrorKind::LinearValueOverwrittenThroughInout { type_name },
                span,
            )
            .with_help(
                "an `inout` binding names the caller's storage; move its linear \
                 value out before the callee reassigns, or pass ownership instead",
            )
        } else {
            CompileError::new(ErrorKind::LinearValueOverwritten { type_name }, span).with_help(
                "consume the old value first (move it, or `@drop` it); a linear \
                 value is never dropped implicitly by an assignment",
            )
        };
        Err(self.attach_infectious_linear_note(err, dest_ty))
    }

    /// Whether the destination place of an assignment provably holds no live
    /// linear value on the current path (RUE-387), so overwriting it destroys
    /// nothing (the spec 3.8:55/56 reinit-after-move idiom).
    ///
    /// True when the exact place was moved out on every path, or — for a whole
    /// linear array — every element was consumed element-wise on every path
    /// (spec 3.8:71). The caller evaluates this on the POST-RHS move state so
    /// that an RHS which itself consumes the old value (`x = f(x)`) counts as a
    /// discharge, matching the RHS-first overwrite-drop order (#968). Only the
    /// whole-variable array shape is tracked per element, so `assigned_path`
    /// must be empty for the element-wise case to apply.
    pub(crate) fn place_linear_discharged(
        &self,
        dest_ty: Type,
        root_var: Spur,
        assigned_path: &[Spur],
        ctx: &AnalysisContext,
    ) -> bool {
        let Some(state) = ctx.moved_vars.get(&root_var) else {
            return false;
        };
        // The exact destination place was moved out on every path.
        if assigned_path.is_empty() {
            if state.full_move_on_all_paths {
                return true;
            }
        } else if state.partial_moves_on_all_paths.contains(assigned_path) {
            return true;
        }
        // A whole linear array consumed element-wise on every path holds no
        // live element to drop.
        assigned_path.is_empty() && self.array_fully_moved_elementwise(dest_ty, state)
    }

    /// Whether every element of an array was WHOLLY moved out on every path
    /// (constant-index element moves, 3.8:68) — the reinit-after-move proof
    /// for a whole-array overwrite (3.8:77).
    ///
    /// This is deliberately stricter than the must-consume discharge
    /// ([`Self::check_array_elementwise_consumption`], which since RUE-1606
    /// also accepts consumption of an element's linear SUB-places): after
    /// `sink(a[0].p)` element 0 is a live husk that the assignment would
    /// still overwrite, so it does not license reassignment — exactly as a
    /// struct husk does not (`c = ...` stays rejected after `sink(c.l)`).
    fn array_fully_moved_elementwise(&self, ty: Type, state: &VariableMoveState) -> bool {
        let TypeKind::Array(array_id) = ty.kind() else {
            return false;
        };
        let (_elem, len) = self.body_type_pool().array_def(array_id);
        (0..len).all(|k| {
            state
                .partial_moves_on_all_paths
                .contains(&vec![index_path_segment(self.body_interner(), k)])
        })
    }

    /// Reject a discarded expression value that carries a linear value
    /// (RUE-176): an expression statement (`make_linear();`) or a loop body
    /// result is dropped without being consumed, which linearity forbids.
    ///
    /// `inst_ref` is the RIR instruction whose span the error points at.
    pub(crate) fn reject_discarded_linear_value(
        &self,
        ty: Type,
        inst_ref: InstRef,
    ) -> CompileResult<()> {
        if !self.type_requires_consumption(ty) {
            return Ok(());
        }
        let err = CompileError::new(
            ErrorKind::LinearValueDiscarded {
                type_name: ty.safe_name_with_pool(Some(self.body_type_pool())),
            },
            self.body_rir_ref().get(inst_ref).span,
        )
        .with_help("bind the value with `let` and consume it, or pass it to a consumer");
        Err(self.attach_infectious_linear_note(err, ty))
    }

    /// If `ty` is a struct that is linear only because a field made it so
    /// (infectious linearity, RUE-40), attach a note explaining the cause.
    /// For an array carrying linear elements, note that the array must be
    /// consumed as a whole.
    pub(crate) fn attach_infectious_linear_note(
        &self,
        err: rue_error::CompileError,
        ty: Type,
    ) -> rue_error::CompileError {
        if let TypeKind::Array(_) = ty.kind() {
            return err.with_note(
                "this is an array whose elements carry linear values; \
                 consume the array as a whole, or move every element out \
                 with constant indices (`consume(xs[0]); consume(xs[1]); ...`)",
            );
        }
        let Some(struct_id) = ty.as_struct() else {
            return err;
        };
        match self.infectious_linear_reason(struct_id).as_ref() {
            Some((field_name, field_type)) => err.with_note(format!(
                "'{}' is linear because field '{}' of type '{}' carries a linear value",
                self.body_type_pool().struct_def(struct_id).name,
                field_name,
                field_type
            )),
            None => err,
        }
    }

    /// Try to record a per-element move out of an array (RUE-186, spec
    /// 3.8:68): `let x = xs[K];` with a CONSTANT index K, rooted directly at
    /// an array variable, moves just element K out. Sibling elements stay
    /// usable; drop elaboration is informed via the marker the caller emits
    /// (see [`Self::emit_element_move_marker`]).
    ///
    /// Returns `Ok(Some(k))` when the move was recorded (the caller must
    /// emit the marker and skip the E0904 rejection), `Ok(None)` when this
    /// index expression is not element-trackable — dynamic index, or the
    /// array is not the trace root — and the caller must keep the
    /// conservative E0904 rejection (spec 7.1:28): with a runtime index sema
    /// cannot know WHICH element moved, so neither use-after-move checking
    /// nor drop suppression could stay sound.
    fn record_element_move_out(
        &mut self,
        trace: &PlaceTrace,
        ctx: &mut AnalysisContext,
        span: Span,
    ) -> CompileResult<Option<i64>> {
        if trace.projections.len() != 1 {
            return Ok(None);
        }
        let Some(k) = trace.projections[0].const_index else {
            return Ok(None);
        };
        if k < 0 {
            // Negative constants are rejected by the bounds check before
            // this is reached; defensive.
            return Ok(None);
        }
        // Moving an element out of a borrow/inout parameter would leave the
        // CALLER's array holed, exactly like a whole or field move.
        self.reject_move_out_of_byref_param(trace.root_var, ctx, span)?;
        let elem_path = vec![index_path_segment(self.body_interner(), k as u64)];
        if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
            // Whole-element move: reject if the element itself, an ancestor, or
            // any descendant subfield was already moved (`arr[0]` can't be
            // moved once `arr[0].s` moved — spec 3.8, RUE-279).
            if let Some(moved_span) = state.is_path_or_descendant_moved(&elem_path) {
                return Err(use_after_move_path_error(
                    self.body_interner(),
                    trace.root_var,
                    &elem_path,
                    span,
                    moved_span,
                ));
            }
        }
        self.reject_move_of_call_loaned_root(trace.root_var, span, ctx)?;
        ctx.moved_vars
            .entry(trace.root_var)
            .or_default()
            .mark_path_moved(&elem_path, span);
        if trace.continues {
            self.record_completed_exclusive_use(trace.root_var, span, ctx);
        }
        Ok(Some(k))
    }

    /// Export a recorded element move (RUE-186) to drop elaboration: wrap
    /// `value` in a [`AirInstData::MarkMoved`] whose place carries a
    /// constant-index projection (the CFG builder resolves it back to the
    /// element index; see `moved_field_path` in `rue-cfg`). Only bases that
    /// drop elaboration would drop get a marker: locals, and Normal (owned)
    /// params — mirroring the field-move marker conditions.
    #[allow(clippy::too_many_arguments)]
    fn emit_element_move_marker(
        &self,
        air: &mut Air,
        trace: &PlaceTrace,
        ctx: &AnalysisContext,
        value: AirRef,
        k: i64,
        elem_type: Type,
        array_type: Type,
        span: Span,
    ) -> CompileResult<AirRef> {
        let droppable_base = match trace.base {
            AirPlaceBase::Local(_) => true,
            AirPlaceBase::Param(_) => ctx
                .params
                .iter()
                .any(|p| p.name == trace.root_var && p.mode == RirParamMode::Normal),
            AirPlaceBase::Accessor(_) | AirPlaceBase::Indirect(_) => false,
        };
        if !droppable_base {
            return Ok(value);
        }
        let (slot, is_param) = match trace.base {
            AirPlaceBase::Local(slot) => (slot, false),
            AirPlaceBase::Param(slot) => (slot, true),
            AirPlaceBase::Accessor(_) | AirPlaceBase::Indirect(_) => {
                unreachable!("accessor places never receive element move markers")
            }
        };
        // A dedicated Const instruction keeps the marker's index resolvable
        // even when the source index expression was a folded constant
        // rather than a literal.
        let idx_const = air.add_inst(AirInst {
            data: AirInstData::Const(k as u64),
            ty: Type::U64,
            span,
        });
        let marker_place = air.make_place(
            trace.base,
            trace.base_type,
            std::iter::once(AirProjection::Index {
                array_type,
                index: idx_const,
            }),
        )?;
        Ok(air.add_inst(AirInst {
            data: AirInstData::MarkMoved {
                value,
                slot,
                is_param,
                place: Some(marker_place),
            },
            ty: elem_type,
            span,
        }))
    }

    /// Reject a read through an array element that is (or may be) moved out
    /// (RUE-186). Element moves only exist for indices rooted directly at an
    /// array variable, so only a position-0 `Index` projection can read
    /// through one. A constant index checks exactly that element's path; a
    /// dynamic index conservatively fails on ANY outstanding partial move of
    /// the root (sema can't know which element is read — soundness).
    fn check_read_through_moved_element(
        &self,
        trace: &PlaceTrace,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<()> {
        let Some(first) = trace.projections.first() else {
            return Ok(());
        };
        if !matches!(first.proj, AirProjection::Index { .. }) {
            return Ok(());
        }
        let Some(state) = ctx.moved_vars.get(&trace.root_var) else {
            return Ok(());
        };
        match first.const_index {
            Some(k) if k >= 0 => {
                let elem_path = vec![index_path_segment(self.body_interner(), k as u64)];
                if let Some(moved_span) = state.is_path_moved(&elem_path) {
                    return Err(use_after_move_path_error(
                        self.body_interner(),
                        trace.root_var,
                        &elem_path,
                        span,
                        moved_span,
                    ));
                }
            }
            _ => {
                if let Some(moved_span) = state.is_any_part_moved() {
                    return Err(use_after_move_path_error(
                        self.body_interner(),
                        trace.root_var,
                        &[],
                        span,
                        moved_span,
                    )
                    .with_note(
                        "the index is not a compile-time constant, so any \
                         moved-out element poisons the whole array",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Reject a write into an array place while one or more of the root
    /// array's elements are moved out (RUE-186, E0480). Re-arming
    /// per-element ownership through an element (or through-element) write
    /// is not supported in general — sema-side reinitialization and the
    /// runtime drop flags would disagree — so the whole array must be
    /// reinitialized instead. The sole exception is an immediate direct
    /// same-element self-move, whose exact path is passed as `except_path` and
    /// re-armed by the caller. Writes into arrays with no outstanding element
    /// moves are unaffected.
    fn reject_write_into_partially_moved_array(
        &self,
        trace: &PlaceTrace,
        ctx: &AnalysisContext,
        span: Span,
        except_path: Option<&[Spur]>,
    ) -> CompileResult<()> {
        // The write must go through the root array (position-0 Index
        // projection); element moves only exist for array roots.
        if !matches!(
            trace.projections.first().map(|p| &p.proj),
            Some(AirProjection::Index { .. })
        ) {
            return Ok(());
        }
        let Some(state) = ctx.moved_vars.get(&trace.root_var) else {
            return Ok(());
        };
        let element_move_span = state.partial_moves.iter().find_map(|(p, s)| {
            if except_path.is_some_and(|except| p.as_slice() == except) {
                return None;
            }
            p.first()
                .filter(|seg| is_index_segment(self.body_interner(), **seg))
                .map(|_| *s)
        });
        let Some(moved_span) = element_move_span else {
            return Ok(());
        };
        let name = self.body_interner().resolve(&trace.root_var).to_string();
        Err(
            CompileError::new(ErrorKind::AssignToPartiallyMovedArray { array: name }, span)
                .with_label("element moved out here", moved_span)
                .with_help("reinitialize the whole array instead (`xs = [...]`)"),
        )
    }

    /// Return the path tracked by a direct constant-index array read, if any.
    ///
    /// This intentionally accepts only `array[K]` rooted directly at a
    /// variable. Nested projections and dynamic indices must remain subject to
    /// the conservative partial-array write rejection.
    fn direct_constant_array_element_path(
        &mut self,
        inst_ref: InstRef,
    ) -> Option<(Spur, FieldPath)> {
        let (base, index) = match &self.body_rir_ref().get(inst_ref).data {
            InstData::IndexGet { base, index } => (*base, *index),
            _ => return None,
        };
        let root = match &self.body_rir_ref().get(base).data {
            InstData::VarRef { name, .. } => *name,
            _ => return None,
        };
        let index = self.try_get_const_index(index)?;
        (index >= 0).then(|| {
            (
                root,
                vec![index_path_segment(self.body_interner(), index as u64)],
            )
        })
    }

    /// Extract the root variable symbol from an expression, if it refers to a variable.
    ///
    /// For inout arguments, we need to track which variable is being passed to detect
    /// when the same variable is passed to multiple inout parameters.
    ///
    /// Returns Some(symbol) for:
    /// - VarRef { name } -> the variable symbol
    /// - FieldGet { base, .. } -> recursively extract from base
    /// - IndexGet { base, .. } -> recursively extract from base
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    pub(crate) fn extract_root_variable(&self, inst_ref: InstRef) -> Option<Spur> {
        root_variable_of(self.body_rir_ref(), inst_ref)
    }

    /// Whether a method receiver rooted at `root` binds a mutable place, for
    /// the `inout self` mut-binding requirement (RUE-15). A `let mut` local is
    /// mutable; an `inout` parameter is mutable (it already names mutable
    /// caller storage); a `mut self` receiver is a mutable by-value binding;
    /// everything else (`let`, plain params, `borrow` params) is not.
    /// Mirrors `PlaceTrace::is_root_mutable`.
    pub(super) fn receiver_root_is_mutable(&self, root: Spur, ctx: &AnalysisContext) -> bool {
        // Locals shadow parameters (RUE-278): a `let mut` rebinding a param
        // name is the mutable binding that later receiver uses see.
        if let Some(local) = ctx.locals.get(&root) {
            return local.is_mut;
        }
        if let Some(param) = ctx.param(root) {
            return param.mode == RirParamMode::Inout || param.is_mut;
        }
        false
    }

    /// Reject a mutation of a collection that an enclosing `for` loop is
    /// iterating (spec 4.8:26, RUE-233). A `for` over a named variable holds a
    /// scoped shared borrow of it for the loop's duration, so mutating that
    /// place inside the body — whole-variable, field, or element — is E0428,
    /// exactly like mutating through an explicit `borrow` parameter.
    pub(crate) fn reject_mutate_iter_borrowed(
        &self,
        root_var: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        if ctx.iter_borrows.contains(&root_var) {
            return Err(CompileError::new(
                ErrorKind::MutateBorrowedValue {
                    variable: self.body_interner().resolve(&root_var).to_string(),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Check exclusivity rules for inout and borrow parameters in a call
    /// (adapter over the shared [`check_exclusive_access_in`], RUE-141).
    pub(crate) fn check_exclusive_access<A>(
        &self,
        args: A,
        call_span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()>
    where
        A: IntoIterator,
        A::Item: std::ops::Deref<Target = RirCallArg>,
    {
        check_exclusive_access_in(
            self.body_rir_ref(),
            self.body_interner(),
            args,
            call_span,
            &|inst_ref| self.place_root_with_accessors(inst_ref, ctx),
        )
    }

    /// Reject recording a MOVE of `root` while an enclosing call's argument
    /// list holds an `inout`/`borrow` loan of the same root (law of
    /// exclusivity, spec 6.1:36, RUE-523).
    ///
    /// The loan spans the entire call, so a by-value use of the loaned
    /// variable in the same argument list — in either order, directly
    /// (`f(inout x, x)`) or nested (`f(inout x, g(x))`) — would hand the
    /// callee an `inout`/`borrow` view of moved-from storage: its destructor
    /// runs in the callee (via the moved-into owner) AND through the loaned
    /// alias — a double free in safe code. Called at every move-record site;
    /// a no-op outside call-argument analysis (the frame stack is empty).
    /// Root-granular, like the other exclusivity rules: even disjoint
    /// projections of one root conflict.
    pub(crate) fn reject_move_of_call_loaned_root(
        &self,
        root: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        for frame in ctx.call_loaned_roots.iter().rev() {
            if let Some((_, kind)) = frame.iter().find(|(r, _)| *r == root) {
                let variable = self.body_interner().resolve(&root).to_string();
                let kw = kind.keyword();
                return Err(CompileError::new(
                    ErrorKind::MoveWhileCallLoaned {
                        variable: variable.clone(),
                        loan_mode: kw.to_string(),
                    },
                    span,
                )
                .with_help(format!(
                    "the `{kw}` access to `{variable}` spans the whole call, so its value \
                     cannot also be moved into it; copy or clone the value into a new \
                     binding before the call"
                )));
            }
        }
        // An accessor-result loan on this root (ADR-0062) spans the enclosing
        // full expression; a move within that extent would leave the borrowed
        // place aliasing moved-from storage.
        self.reject_accessor_loan_conflict(root, "by value (move)", span, ctx)
    }

    /// Reject an exclusive use — `inout`, mutation, or move — of a root with
    /// an active accessor-result loan in the same full expression (ADR-0062,
    /// E0259). Shared and exclusive accessor loans both exclude another
    /// exclusive access for their full extent.
    pub(crate) fn reject_accessor_loan_conflict(
        &self,
        root: Spur,
        conflict: &'static str,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        if let Some((_, loan_span, _kind)) =
            ctx.expression_loans.iter().find(|(r, _, _)| *r == root)
        {
            return Err(CompileError::new(
                ErrorKind::AccessorLoanConflict {
                    variable: self.body_interner().resolve(&root).to_string(),
                    conflict,
                },
                span,
            )
            .with_label("accessor result borrows the value here", *loan_span));
        }
        Ok(())
    }

    /// Remember an exclusive use after its operation has completed. A
    /// nested call's live loan frame disappears when that call returns, but
    /// its exclusive use still conflicts with a later accessor result in the
    /// enclosing full expression.
    pub(crate) fn record_completed_exclusive_use(
        &self,
        root: Spur,
        span: Span,
        ctx: &mut AnalysisContext,
    ) {
        if !ctx
            .expression_exclusive_uses
            .iter()
            .any(|(used_root, _)| *used_root == root)
        {
            ctx.expression_exclusive_uses.push((root, span));
        }
    }

    pub(crate) fn reject_accessor_shared_loan_conflict(
        &self,
        root: Spur,
        conflict: &'static str,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        if let Some((_, loan_span, _kind)) = ctx
            .expression_loans
            .iter()
            .find(|(r, _, kind)| *r == root && *kind == CallLoanKind::Inout)
        {
            return Err(CompileError::new(
                ErrorKind::AccessorLoanConflict {
                    variable: self.body_interner().resolve(&root).to_string(),
                    conflict,
                },
                span,
            )
            .with_label(
                "exclusive accessor result borrows the value here",
                *loan_span,
            ));
        }
        Ok(())
    }

    /// Reject a by-value move of a non-trivially-droppable value out of a
    /// place reached through an accessor result (ADR-0062, E0258): the
    /// result is a borrowed place, not an owner, so moving an owning value
    /// out of it would mint an aliasing second owner. Exclusive writes remain
    /// legal after receiver mutability and loan checks.
    pub(super) fn reject_accessor_place_move(
        &self,
        trace: &PlaceTrace,
        moved_type: Type,
        span: Span,
    ) -> CompileResult<()> {
        if trace.via_accessor {
            return Err(CompileError::new(
                ErrorKind::AccessorResultMoved {
                    ty: moved_type.safe_name_with_pool(Some(self.body_type_pool())),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Every accessor result the value of `operand` can be: the accessor call
    /// itself, or — when `operand` is a *join* — the results its arms
    /// contribute, in leftmost-first order (ADR-0062, RUE-1678).
    ///
    /// A block tail, an `if`/`else` arm, and a `match` arm each materialize
    /// their value AS the value of the enclosing expression, so an accessor
    /// result reached through one is still the borrowed place that expression
    /// consumes: `let b = if c { p.at(i) } else { 0 };` binds exactly the
    /// second-class place `let b = p.at(i);` binds. Only the accessor CALL is
    /// registered in `ctx.accessor_call_insts` (a join has no AIR shape of its
    /// own to peel), so the join relation is decided structurally here, from
    /// the RIR, and composes through nesting — an `if` inside a `match` arm
    /// inside a block reports the accessor at the bottom.
    ///
    /// Arms contribute independently: a join whose arms call two different
    /// accessors yields both, and a join with one accessor arm and one
    /// ordinary arm still yields that one, because the accessor path escapes.
    fn accessor_results_of(&self, operand: InstRef, ctx: &AnalysisContext) -> Vec<(Spur, Spur)> {
        // Overwhelmingly common case: the body expanded no accessor at all,
        // so no join can carry one and nothing needs walking or allocating.
        if ctx.accessor_call_insts.is_empty() {
            return Vec::new();
        }
        let rir = self.body_rir_ref();
        let mut found = Vec::new();
        // Depth-first over a RIR expression tree (acyclic by construction),
        // children pushed in reverse so arms are visited left to right.
        let mut pending = vec![operand];
        while let Some(current) = pending.pop() {
            if let Some(direct) = ctx.accessor_call_insts.get(&current) {
                found.push(*direct);
                continue;
            }
            match &rir.get(current).data {
                InstData::Block { instructions } => {
                    if let Some(tail) = rir.block_insts(instructions).values().last() {
                        pending.push(tail);
                    }
                }
                InstData::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    // An `if` without `else` is unit-typed and carries no
                    // value, so only the two-armed form can join a result.
                    if let Some(else_block) = else_block {
                        pending.push(*else_block);
                        pending.push(*then_block);
                    }
                }
                InstData::Match { arms, .. } => {
                    // The arm iterator is not double-ended, so collect the
                    // bodies before reversing them onto the stack.
                    let bodies: Vec<InstRef> =
                        rir.match_arms(arms).iter().map(|(_, body)| body).collect();
                    pending.extend(bodies.into_iter().rev());
                }
                _ => {}
            }
        }
        found
    }

    /// Does the value of `operand` carry an accessor result out of a join
    /// (ADR-0062, RUE-1678)? Used by the `if`/`match` arm analyses to decide
    /// whether an arm's accessor loan outlives the arm's nested-full-expression
    /// boundary; see [`Self::accessor_results_of`].
    fn yields_accessor_result(&self, operand: InstRef, ctx: &AnalysisContext) -> bool {
        !self.accessor_results_of(operand, ctx).is_empty()
    }

    /// Carry an `if`/`match` arm's accessor loans into the join's enclosing
    /// full expression, when the arm's value is what the join yields
    /// (ADR-0062 6.6:10, RUE-1678).
    ///
    /// Each arm is analyzed as a nested full expression, so `loans` — taken
    /// with `nested_expression_loans` just before that boundary closed — has
    /// already been discarded by the time this runs. For an arm whose value
    /// is an accessor result the discard is wrong: the loan's extent is the
    /// full expression the JOIN belongs to, so an exclusive use of the same
    /// root anywhere in it is still E0259 (`using(if c { g.at(0) } else { 0 },
    /// inout g)`). Arms are alternatives rather than siblings, so every arm's
    /// loans are readmitted only after the last arm has been analyzed —
    /// readmitting earlier would make one arm's loan conflict with the next
    /// arm's accessor call on the same root.
    pub(crate) fn readmit_arm_accessor_loans(
        &self,
        ctx: &mut AnalysisContext,
        arm: InstRef,
        loans: Vec<(Spur, Span, CallLoanKind)>,
    ) {
        if loans.is_empty() || !self.yields_accessor_result(arm, ctx) {
            return;
        }
        ctx.readmit_expression_loans(loans);
    }

    /// Reject the escape of an accessor result (ADR-0062): `operand` is the
    /// RIR expression consumed at an escape-shaped site, and if its value is
    /// an accessor result — the call itself, or one carried out of an
    /// `if`/`match`/block join — the borrowed place would outlive its
    /// enclosing full expression there. A join over two accessors escapes
    /// either way, so the diagnostic names the leftmost contributing arm.
    pub(crate) fn reject_accessor_result_escape(
        &self,
        operand: InstRef,
        site: AccessorEscapeSite,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        let results = self.accessor_results_of(operand, ctx);
        let Some((method, root)) = results.first() else {
            return Ok(());
        };
        let method = self.body_interner().resolve(method).to_string();
        let root = self.body_interner().resolve(root).to_string();
        let kind = match site {
            AccessorEscapeSite::Return => ErrorKind::AccessorResultReturned { method, root },
            AccessorEscapeSite::Store => ErrorKind::AccessorResultStored { method, root },
            AccessorEscapeSite::Let => ErrorKind::AccessorResultBound { method, root },
            AccessorEscapeSite::Capture => ErrorKind::AccessorResultCaptured { method, root },
        };
        Err(CompileError::new(kind, span).with_help(
            "read the borrowed place, project a field from it, pass it as a `borrow` \
             argument, or compare it — all within the expression that calls the accessor",
        ))
    }

    /// Is `operand` a *direct* reference to a `borrow str` / `inout str`
    /// parameter — i.e. a borrowed `str` *view* value (ADR-0043 two-types
    /// model, RUE-386)?
    ///
    /// Only a whole-value `VarRef` to such a parameter is a view value:
    /// reading *through* the view (`s.len()`, `s[i]`) never yields a `str`, and
    /// a `let`-rebind of the view is itself blocked at its binding site, so
    /// there is never a second first-class root to chase. This keeps the check
    /// structural (one instruction shape), never a dataflow — exactly what the
    /// no-lifetimes spine requires.
    fn str_view_operand_mode(
        &self,
        operand: InstRef,
        ctx: &AnalysisContext,
    ) -> Option<RirParamMode> {
        if let InstData::VarRef { name, .. } = self.body_rir_ref().get(operand).data {
            // Locals shadow parameters. Without this guard a local reusing a
            // view parameter's name would inherit its second-class provenance.
            if ctx.locals.contains_key(&name) {
                return None;
            }
            return ctx
                .params
                .iter()
                .find(|p| {
                    p.name == name
                        && matches!(p.mode, RirParamMode::Borrow | RirParamMode::Inout)
                        && self.is_str_struct(p.ty)
                })
                .map(|p| p.mode);
        }
        None
    }

    pub(crate) fn is_str_view_operand(&self, operand: InstRef, ctx: &AnalysisContext) -> bool {
        self.str_view_operand_mode(operand, ctx).is_some()
    }

    /// Enforce the ADR-0043 two-types string model at a *first-class* `str`
    /// destination (RUE-386): reject a string *buffer* (`StrBuf`/`Str(N)`, the
    /// growable/fixed rungs) or a borrowed `str` *view* being stored as a
    /// first-class `str`. `found` is the operand's analyzed type; `operand` is
    /// its source instruction (used to recognise a view). Callers invoke this
    /// only when the destination type is the bare `str` (not `Str(N)`).
    ///
    /// A buffer's bytes live in caller-owned local/heap storage and a view
    /// aliases a borrow's scope; either escaping as a first-class `str` (which
    /// is `Copy`, storable, and returnable) dangles once the storage is freed —
    /// the verified RUE-386 segfault. Buffers coerce only to `borrow str` /
    /// `inout str`; views may only be read or re-borrowed.
    pub(crate) fn reject_non_first_class_str(
        &self,
        operand: InstRef,
        found: Type,
        site: FirstClassStrSite,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        if self.is_strbuf(found) || self.is_str_fixed_struct(found) {
            let found_name = found.safe_name_with_pool(Some(self.body_type_pool()));
            let mut err = CompileError::new(
                ErrorKind::BufferNotFirstClassStr {
                    found: found_name,
                    site: site.describe().to_string(),
                },
                span,
            );
            err = if site == FirstClassStrSite::Param {
                err.with_help(
                    "parameter type `str` accepts only string literals; did you mean `borrow str`?",
                )
            } else {
                err.with_help(
                    "a string buffer can be viewed with `borrow str` / `inout str`, \
                     but not stored as a first-class `str`",
                )
            };
            return Err(err);
        }
        if self.is_str_view_operand(operand, ctx) {
            return Err(CompileError::new(
                ErrorKind::StrViewNotFirstClass {
                    site: site.describe().to_string(),
                },
                span,
            )
            .with_help(
                "a `borrow str` / `inout str` view is second-class and cannot escape the call; \
                 it can only be read (`.len()`, byte indexing) or re-borrowed",
            ));
        }
        Ok(())
    }

    /// Validate the source of a bare `inout str` view (ADR-0043 two-types
    /// model, RUE-386). A locally-backed `StrBuf`/`Str(N)` is accepted, as is
    /// forwarding an existing `inout str` parameter (which preserves that
    /// local provenance). A first-class/static `str` is E0496; an unrelated
    /// type is the ordinary E0206 type mismatch.
    pub(crate) fn validate_inout_str_operand(
        &self,
        operand: InstRef,
        expected: Type,
        found: Type,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        if found.is_never() || found.is_error() {
            return Ok(());
        }
        if self.is_strbuf(found)
            || self.is_str_fixed_struct(found)
            || self.str_view_operand_mode(operand, ctx) == Some(RirParamMode::Inout)
        {
            return Ok(());
        }
        if self.is_str_struct(found) {
            return Err(
                CompileError::new(ErrorKind::InoutStrRequiresLocalBuffer, span).with_help(
                    "`inout str` requires a local `StrBuf` or `Str(N)`; \
                     a first-class `str` cannot be exclusively viewed",
                ),
            );
        }
        Err(self.type_mismatch_error(expected, found, span))
    }

    /// The tail (value) expression of a RIR block, descending through nested
    /// blocks. Used to locate the *implicit-return* operand of a `str`-typed
    /// function body so the two-types model (RUE-386) can reject a buffer or a
    /// borrowed view escaping via the tail value. A non-block body is its own
    /// tail; an empty block returns itself (its value is unit, harmless here).
    pub(crate) fn rir_block_tail_expr(&self, inst: InstRef) -> InstRef {
        let mut current = inst;
        loop {
            match &self.body_rir_ref().get(current).data {
                InstData::Block { instructions } => {
                    match self
                        .body_rir_ref()
                        .block_insts(instructions)
                        .values()
                        .last()
                    {
                        Some(tail) => current = tail,
                        None => return current,
                    }
                }
                _ => return current,
            }
        }
    }

    /// Analyze call arguments, materializing a fat-pointer slice value for any
    /// argument whose parameter is a slice type (ADR-0043, RUE-322).
    ///
    /// `borrow arr` (where `arr: [T; N]`) passed to a `borrow s: [T]` parameter
    /// is coerced to a by-value slice `{ptr: @raw(arr[0]), len: N}`, which flows
    /// through the existing by-value aggregate ABI (the parameter is by-value —
    /// see the host's parameter setup). All other arguments retain
    /// the ordinary by-value/by-reference analysis in this same chokepoint.
    ///
    /// A `borrow` operand that denotes no existing place is elaborated here
    /// into one that does (RUE-953); see
    /// [`Self::elaborate_borrow_operand`] for the two mechanisms and for why
    /// the returned [`CallOperands::temp_scope`] must wrap the emitted call.
    pub(crate) fn analyze_call_args_coerced<A>(
        &mut self,
        air: &mut Air,
        args: A,
        param_types: &[Type],
        param_modes: &[RirParamMode],
        call_may_continue: bool,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<CallOperands>
    where
        A: ExactSizeIterator<Item = rue_rir::RirCallArg> + Clone,
    {
        // Loan-frame discipline (RUE-523): a by-value move of a root this call
        // passes `inout`/`borrow` conflicts in either argument order.
        let frame: Vec<(Spur, CallLoanKind)> = args
            .clone()
            .filter_map(|arg| {
                let kind = if arg.is_inout() {
                    CallLoanKind::Inout
                } else if arg.is_borrow() {
                    CallLoanKind::Borrow
                } else {
                    return None;
                };
                root_variable_of(self.body_rir_ref(), arg.value)
                    .or_else(|| {
                        // Both shared and exclusive accessor-call place chains
                        // root their loan at the receiver (ADR-0062/RUE-1016).
                        self.place_root_with_accessors(arg.value, ctx)
                    })
                    .map(|root| (root, kind))
            })
            .collect();
        // An `inout` loan on a root an accessor result borrows in the same
        // full expression violates exclusivity (ADR-0062, E0259). Checked
        // three times: here at frame construction (loans taken earlier in the
        // enclosing expression, e.g. by an already-analyzed sibling of an
        // outer call), by accessor expansion's frame check (an accessor
        // expanded under an outer call's live by-ref frame), and again after
        // this argument list is analyzed (loans registered by THIS call's own
        // accessor arguments — the direct sibling forms, RUE-1593).
        for arg in args.clone() {
            if !arg.is_inout() && !arg.is_borrow() {
                continue;
            }
            if let Some(root) = root_variable_of(self.body_rir_ref(), arg.value) {
                if arg.is_inout() {
                    self.reject_accessor_loan_conflict(
                        root,
                        "as `inout`",
                        self.body_rir_ref().get(arg.value).span,
                        ctx,
                    )?;
                } else {
                    self.reject_accessor_shared_loan_conflict(
                        root,
                        "as `borrow`",
                        self.body_rir_ref().get(arg.value).span,
                        ctx,
                    )?;
                }
            }
        }
        let pushed = !frame.is_empty();
        if pushed {
            ctx.call_loaned_roots.push(frame);
        }
        let result =
            self.analyze_call_args_coerced_inner(air, args.clone(), param_types, param_modes, ctx);
        if pushed {
            ctx.call_loaned_roots.pop();
        }
        let result = result?;
        // Re-check after the argument list is analyzed (RUE-1593): an accessor
        // expanded among THIS call's own arguments registered its loan in
        // `ctx.expression_loans` only during the inner analysis, after the
        // pre-frame check above ran. A direct sibling by-ref use of the same
        // root — `use(v.get_ref(i), inout v)` or `use(inout v, v.get_ref(i))` —
        // is an exclusive use of the borrowed root within the same full
        // expression and is rejected in either argument order (spec 6.6:10,
        // 6.6:16, E0259). Args whose place roots through an accessor chain
        // (`f(inout v.get_mut(i))`) have no plain root variable here and are
        // governed by accessor expansion's own frame check instead.
        for arg in args.clone() {
            if !arg.is_inout() && !arg.is_borrow() {
                continue;
            }
            if let Some(root) = root_variable_of(self.body_rir_ref(), arg.value) {
                if arg.is_inout() {
                    self.reject_accessor_loan_conflict(
                        root,
                        "as `inout`",
                        self.body_rir_ref().get(arg.value).span,
                        ctx,
                    )?;
                } else {
                    self.reject_accessor_shared_loan_conflict(
                        root,
                        "as `borrow`",
                        self.body_rir_ref().get(arg.value).span,
                        ctx,
                    )?;
                }
            }
        }
        // The exclusive use is complete once this call's argument list has
        // finished and all same-call accessor checks have succeeded. Retain
        // it for the enclosing full expression so a later accessor result
        // still observes the conflict.
        if result.continues && call_may_continue {
            for arg in args {
                if arg.is_inout()
                    && let Some(root) = self.extract_root_variable(arg.value)
                {
                    self.record_completed_exclusive_use(
                        root,
                        self.body_rir_ref().get(arg.value).span,
                        ctx,
                    );
                }
            }
        }
        Ok(result)
    }

    /// The argument loop behind [`Self::analyze_call_args_coerced`], factored
    /// out so the loan frame is popped on every exit path.
    fn analyze_call_args_coerced_inner<A>(
        &mut self,
        air: &mut Air,
        args: A,
        param_types: &[Type],
        param_modes: &[RirParamMode],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<CallOperands>
    where
        A: ExactSizeIterator<Item = rue_rir::RirCallArg>,
    {
        let mut air_args = Vec::with_capacity(args.len());
        let mut temp_scope: Vec<AirRef> = Vec::new();
        let mut continues = true;
        for (i, arg) in args.enumerate() {
            let reachable_edges_before_arg = ctx.loop_break_stack.clone();
            // A `str` parameter (ADR-0043 Phase 3, RUE-324) is a first-class
            // 2-word value, not a `borrow`-materialized fat pointer. A string
            // literal argument materializes as a `str` under the expected type;
            // a `str` variable is passed through by value. Either way it flows
            // through the by-value aggregate ABI, so it is handled here before
            // the array→slice `borrow` coercion below.
            //
            // Exception (RUE-385): `inout str` must pass the caller's storage by
            // address. Unlike a slice view, `str` is first-class and
            // reassignable, so an assignment to the parameter rebinds the
            // caller's fat pointer via ParamStore.
            let param_ty = param_types[i];
            let param_mode = param_modes[i];
            let is_str_param = self.is_str_like(param_ty);
            let is_inout_str_param =
                param_mode == RirParamMode::Inout && self.is_str_struct(param_ty);
            let is_exact_str_fixed_ref =
                matches!(param_mode, RirParamMode::Borrow | RirParamMode::Inout)
                    && self.is_str_fixed_struct(param_ty);
            if is_str_param && !is_inout_str_param && !is_exact_str_fixed_ref {
                let str_ty = param_ty;
                // A `borrow` argument to a `borrow s: str` parameter views an
                // existing string place — a `StrBuf`, `Str(N)`, `str`, or a
                // re-borrowed view (ADR-0043 two-types model). The source is
                // borrowed (never moved) and a `StrBuf` source's 3-word header
                // is narrowed to the 2-word `{ptr, len}` view here (RUE-559).
                // An operand that names no place is elaborated first (RUE-953):
                // a promoted string literal already *is* the static-backed
                // view, and any other value is viewed through the temporary
                // that owns it for the call.
                if arg.is_borrow() && self.is_str_struct(str_ty) {
                    let (value, storage_live) =
                        self.coerce_borrow_str_operand_to_view(air, &arg, str_ty, ctx)?;
                    temp_scope.extend(storage_live);
                    air_args.push(AirCallArg {
                        value,
                        // The 2-word view is passed BY VALUE (multi-slot
                        // aggregate), exactly like a slice fat pointer.
                        mode: AirArgMode::Normal,
                    });
                    continue;
                }
                let prev_expected = ctx.expected_type.replace(str_ty);
                let arg_result = self.analyze_inst(air, arg.value, ctx);
                ctx.expected_type = prev_expected;
                let arg_result = arg_result?;
                if !continues {
                    Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_arg);
                }
                continues &= arg_result.continues;
                // `Str(N)` is a nominal fixed-capacity value, not a bare
                // string view. Contextual literals materialize directly as
                // the expected capacity above; every other value must retain
                // exact capacity identity (RUE-636). This check is semantic
                // across by-value calls as well as the exact by-reference path
                // below.
                if self.is_str_fixed_struct(str_ty) && !arg_result.ty.can_coerce_to(&str_ty) {
                    return Err(self.type_mismatch_error(
                        str_ty,
                        arg_result.ty,
                        self.body_rir_ref().get(arg.value).span,
                    ));
                }
                // Two-types model (ADR-0043, RUE-386): a bare `str` parameter
                // (Normal mode) requires a first-class `str`; a buffer or a
                // borrowed view passed here would escape and dangle. A
                // `borrow str` parameter (Borrow mode) is the sanctioned view
                // and accepts a buffer, so it is exempt.
                if param_mode == RirParamMode::Normal && self.is_str_struct(str_ty) {
                    self.reject_non_first_class_str(
                        arg.value,
                        arg_result.ty,
                        FirstClassStrSite::Param,
                        self.body_rir_ref().get(arg.value).span,
                        ctx,
                    )?;
                    if !arg_result.ty.can_coerce_to(&str_ty) {
                        return Err(self.type_mismatch_error(
                            str_ty,
                            arg_result.ty,
                            self.body_rir_ref().get(arg.value).span,
                        ));
                    }
                }
                air_args.push(AirCallArg {
                    value: arg_result.air_ref,
                    mode: AirArgMode::Normal,
                });
                continue;
            }

            let is_slice_param = param_types
                .get(i)
                .is_some_and(|pt| self.slice_element_type(*pt).is_some());
            if is_slice_param && !is_inout_str_param && !is_exact_str_fixed_ref {
                let slice_ty = param_types[i];
                let value = self.coerce_borrow_array_to_slice(air, &arg, slice_ty, ctx)?;
                air_args.push(AirCallArg {
                    value,
                    // The fat pointer is passed BY VALUE (multi-slot aggregate).
                    mode: AirArgMode::Normal,
                });
                continue;
            }

            // A `borrow` operand that denotes no existing place — a literal, a
            // named constant, a call result, an arithmetic expression — is
            // elaborated into one (RUE-953). The decision is taken here,
            // before analysis, so the operand is analyzed as an ordinary value
            // under the parameter's expected type instead of being traced as a
            // place. `inout` is unchanged: an exclusive loan still requires
            // caller-visible storage.
            let elaborates_borrow =
                arg.is_borrow() && !self.borrow_operand_names_place(arg.value, ctx);

            let byref_root = if !elaborates_borrow && (arg.is_inout() || arg.is_borrow()) {
                // A `borrow` argument may be an accessor-call place chain
                // (ADR-0062): it borrows the accessor receiver's root.
                let root = match root_variable_of(self.body_rir_ref(), arg.value) {
                    Some(root) => root,
                    None => match self
                        .place_root_with_accessors(arg.value, ctx)
                        .filter(|_| arg.is_borrow() || arg.is_inout())
                    {
                        Some(root) => root,
                        None => {
                            return Err(require_byref_place_arg(self.body_rir_ref(), &arg)
                                .expect_err(
                                    "argument without a place root fails the byref check",
                                ));
                        }
                    },
                };
                if arg.is_inout()
                    && !ctx.locals.contains_key(&root)
                    && ctx
                        .params
                        .iter()
                        .any(|p| p.name == root && p.mode == RirParamMode::Borrow)
                {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: self.body_interner().resolve(&root).to_string(),
                        },
                        self.body_rir_ref().get(arg.value).span,
                    ));
                }
                Some(root)
            } else {
                None
            };
            let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
            // An elaborated operand is materialized at the parameter's type, so
            // a string literal at a `borrow StrBuf` position becomes the
            // literal-backed `cap == 0` header and an enum-typed fallible
            // intrinsic names its registry result — the same contextual
            // materialization a `let` with that annotation performs.
            let prev_expected = if elaborates_borrow
                && (param_ty.is_enum() || self.is_str_like(param_ty) || self.is_strbuf(param_ty))
            {
                Some(ctx.expected_type.replace(param_ty))
            } else {
                None
            };
            let arg_result = self.analyze_inst(air, arg.value, ctx);
            if let Some(previous) = prev_expected {
                ctx.expected_type = previous;
            }
            ctx.byref_arg_root = prev_byref_root;
            let mut arg_result = arg_result?;
            if !continues {
                Self::restore_reachable_loop_edges(ctx, &reachable_edges_before_arg);
            }
            continues &= arg_result.continues;
            if elaborates_borrow {
                let span = self.body_rir_ref().get(arg.value).span;
                let elaborated = self.elaborate_borrow_operand(
                    air,
                    arg.value,
                    arg_result.air_ref,
                    arg_result.ty,
                    span,
                    ctx,
                )?;
                temp_scope.push(elaborated.storage_live);
                let value =
                    air.add_block(&[elaborated.alloc], elaborated.load, arg_result.ty, span)?;
                air_args.push(AirCallArg {
                    value,
                    mode: AirArgMode::Borrow,
                });
                continue;
            }
            if arg.is_inout() || arg.is_borrow() {
                // An inlined accessor argument arrives as a guards block whose
                // tail is the place read (ADR-0062); peel it for the address
                // check, like the `inout str` view materialization below.
                let addressable_probe = if ctx.accessor_call_insts.contains_key(&arg.value) {
                    let (place, prefix) = self.peel_projected_rvalue_scope(air, arg_result.air_ref);
                    if !prefix.is_empty() {
                        arg_result = AnalysisResult::new(place, arg_result.ty);
                        temp_scope.extend(prefix);
                    }
                    place
                } else {
                    arg_result.air_ref
                };
                self.require_addressable_read(
                    air,
                    addressable_probe,
                    arg.is_inout(),
                    self.body_rir_ref().get(arg.value).span,
                )?;
            }
            // Two-types model (ADR-0043, RUE-386): an `inout str` parameter is
            // an *exclusive* view and requires local provenance — a first-class
            // / static `str` value is never a legal exclusive operand (closes
            // the write-to-`.rodata` and Copy-two-roots aliasing holes).
            if is_inout_str_param {
                self.validate_inout_str_operand(
                    arg.value,
                    param_ty,
                    arg_result.ty,
                    self.body_rir_ref().get(arg.value).span,
                    ctx,
                )?;
                // RUE-1066: a `StrBuf` source no longer shares `str`'s `{ptr,
                // len}` prefix now that its growable storage nests in a RawBuf
                // core (`[buf, cap, len]`). Passing the raw 3-word storage by
                // reference would let the callee read `cap` as `len`. Narrow to
                // a method-routed `{ptr, len}` view materialized in a temporary
                // and pass an exclusive reference to that instead: the view's
                // pointer is `as_ptr()` (the real buffer), so byte writes still
                // reach the caller's storage, while `len` reads the correct
                // word. Whole-view rebinding is already rejected
                // (StrViewReassignment), so the snapshot length stays valid.
                if self.is_strbuf(arg_result.ty) {
                    let span = self.body_rir_ref().get(arg.value).span;
                    let (view, prefix) =
                        self.strbuf_text_view(air, arg_result.air_ref, arg_result.ty, span, ctx)?;
                    let view =
                        self.wrap_value_with_temp_scope(air, view, param_ty, span, prefix)?;
                    let (load, mat_prefix) =
                        self.materialize_borrow_argument(air, view, param_ty, span, ctx)?;
                    let value =
                        self.wrap_value_with_temp_scope(air, load, param_ty, span, mat_prefix)?;
                    air_args.push(AirCallArg {
                        value,
                        mode: AirArgMode::Inout,
                    });
                    continue;
                }
            }
            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(CallOperands {
            args: air_args,
            temp_scope,
            continues,
        })
    }

    /// Build a by-value slice `{ptr, len}` value from a `borrow arr` argument
    /// (ADR-0043, RUE-322). The array must be a place whose element type matches
    /// the slice's; the pointer word is `@raw(arr[0])` and the length word is
    /// the array's compile-time length `N`.
    fn coerce_borrow_array_to_slice(
        &mut self,
        air: &mut Air,
        arg: &RirCallArg,
        slice_ty: Type,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AirRef> {
        use crate::inst::{AirInstData, AirProjection};

        let span = self.body_rir_ref().get(arg.value).span;
        let slice_struct_id = slice_ty
            .as_struct()
            .expect("slice type is a synthetic struct");
        let slice_elem = self
            .slice_element_type(slice_ty)
            .expect("slice type has an element");
        let ptr_ty = self.body_type_pool().struct_def(slice_struct_id).fields[0].ty;

        // A slice argument must be `borrow arr` (the shared form; `inout` slices
        // are a later phase). The array is borrowed (address taken), not moved.
        if !arg.is_borrow() {
            return Err(CompileError::new(ErrorKind::BorrowKeywordMissing, span));
        }
        let root = require_byref_place_arg(self.body_rir_ref(), arg)?;
        let prev_byref_root = ctx.byref_arg_root.replace(root);
        let trace = self.try_trace_place(arg.value, air, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let trace = trace?.ok_or_else(|| CompileError::new(ErrorKind::BorrowNonLvalue, span))?;

        let arr_ty = trace.result_type();

        // Forwarding an existing slice (`inner(borrow s)` where `s: [T]`): the
        // fat pointer is already built, so read it by value (a slice is `@copy`)
        // and pass it through — no re-materialization.
        if self.types_equivalent(arr_ty, slice_ty) {
            let projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            let place_ref = air.make_place(trace.base, trace.base_type, projs)?;
            let val = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: slice_ty,
                span,
            });
            return Ok(val);
        }

        let (arr_elem, arr_len) = match arr_ty.as_array() {
            Some(id) => self.body_type_pool().array_def(id),
            None => {
                // Only whole-array borrow-to-slice is supported in Phase 1.
                return Err(self.type_mismatch_error(slice_ty, arr_ty, span));
            }
        };
        if !self.types_equivalent(arr_elem, slice_elem) {
            return Err(self.type_mismatch_error(slice_ty, arr_ty, span));
        }

        // A non-slot-width element's compact memory image is not byte-identical
        // to the full-slot representation used by frame arrays. For narrow
        // scalars this makes the slice's compact element stride differ from the
        // frame stride; aggregate elements can also differ in tags, fields, or
        // padding. Keep the deliberate refusal, but report it at the source-level
        // coercion boundary rather than letting the synthesized pointer reach the
        // backend's raw-pointer safety gate (RUE-1595). Empty arrays remain valid:
        // their null pointer is never dereferenced.
        if arr_len != 0 && !crate::is_slot_identical_layout(self.body_type_pool(), arr_elem) {
            return Err(CompileError::new(
                ErrorKind::SliceFrameArrayNotSupported {
                    element_type: self.format_type_name(arr_elem),
                },
                span,
            ));
        }

        let zero_ref = air.add_inst(AirInst {
            data: AirInstData::Const(0),
            ty: Type::U64,
            span,
        });
        let ptr_ref = if arr_len == 0 {
            // A zero-length slice never dereferences its pointer: every read is
            // guarded by `i < len`, and `len` is 0. Do not form `@raw(arr[0])`
            // for `[T; 0]`; that is not a valid place and underflows in some
            // codegen paths. Use a conventional null pointer word instead.
            air.add_intrinsic(
                None,
                self.known_symbols().int_to_ptr,
                &[zero_ref],
                ptr_ty,
                span,
            )?
        } else {
            // ptr word = @raw(arr[0]) : ptr const T. Build a place read of
            // element 0 and take its address, exactly as source `@raw(arr[0])`
            // would.
            let mut projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            projs.push(AirProjection::Index {
                array_type: arr_ty,
                index: zero_ref,
            });
            let place_ref = air.make_place(trace.base, trace.base_type, projs)?;
            let elem0_read = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: arr_elem,
                span,
            });
            air.add_intrinsic(None, self.known_symbols().raw, &[elem0_read], ptr_ty, span)?
        };

        // len word = N (compile-time array length).
        let len_ref = air.add_inst(AirInst {
            data: AirInstData::Const(arr_len),
            ty: Type::U64,
            span,
        });

        // Materialize the fat pointer `{ptr, len}` struct value.
        let slice_val = air.add_struct_init(
            slice_struct_id,
            &[ptr_ref, len_ref],
            &[0, 1],
            slice_ty,
            span,
        )?;
        Ok(slice_val)
    }

    /// Build a by-value `str` view `{ptr, len}` from a `borrow` argument whose
    /// parameter is `borrow s: str` (ADR-0043 two-types model, RUE-559).
    ///
    /// The source place is *borrowed*: `ctx.byref_arg_root` is set while it is
    /// traced, so no move is recorded and the buffer stays owned by (and is
    /// dropped in) the caller — which also keeps the RUE-523 loan-frame check
    /// from misreading the view construction as a move of its own loan.
    ///
    /// - A `str` / `Str(N)` source (including a re-borrowed `borrow str`
    ///   parameter, spec 3.7:58) already has the 2-word view shape and is read
    ///   by value.
    /// - A `StrBuf` source is a 3-word `{ptr, len, cap}` buffer header; the
    ///   view copies exactly the `{ptr, len}` prefix. Passing the raw 3-word
    ///   value through the 2-slot by-value parameter ABI was the RUE-559
    ///   miscompile: the callee read `cap` as the length and dereferenced the
    ///   length as the data pointer (segfault on indexing).
    ///
    /// An operand that names no place is elaborated first (RUE-953) and the
    /// returned `StorageLive`, when there is one, must wrap the call so a
    /// temporary outlives the view built from it.
    fn coerce_borrow_str_operand_to_view(
        &mut self,
        air: &mut Air,
        arg: &RirCallArg,
        str_ty: Type,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<(AirRef, Option<AirRef>)> {
        use crate::inst::{AirInstData, AirProjection};

        let span = self.body_rir_ref().get(arg.value).span;
        if !self.borrow_operand_names_place(arg.value, ctx) {
            return self.elaborate_borrow_str_operand(air, arg, str_ty, ctx);
        }
        let root = require_byref_place_arg(self.body_rir_ref(), arg)?;
        let prev_byref_root = ctx.byref_arg_root.replace(root);
        let trace = self.try_trace_place(arg.value, air, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let trace = trace?.ok_or_else(|| CompileError::new(ErrorKind::BorrowNonLvalue, span))?;

        let src_ty = trace.result_type();

        // `str` / `Str(N)` sources share the 2-word `{ptr, len}` shape: read
        // the fat pointer by value (it is `@copy`) and pass it through.
        if self.is_str_like(src_ty) {
            let projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            let place_ref = air.make_place(trace.base, trace.base_type, projs)?;
            return Ok((
                air.add_inst(AirInst {
                    data: AirInstData::PlaceRead { place: place_ref },
                    ty: str_ty,
                    span,
                }),
                None,
            ));
        }

        // `StrBuf` source: narrow the buffer header to the `{ptr, len}` view.
        // The prefix is read through the trusted `as_ptr()`/`len()` accessors
        // rather than by projecting fields 0/1, so this coercion is independent
        // of StrBuf's internal layout (RUE-1066). Reading the whole StrBuf as a
        // `PlaceRead` keeps it addressable, so the accessor calls borrow it in
        // place without moving it.
        if self.is_strbuf(src_ty) {
            let projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            let place_ref = air.make_place(trace.base, trace.base_type, projs)?;
            let strbuf_read = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: src_ty,
                span,
            });
            let (view, prefix) = self.strbuf_text_view(air, strbuf_read, src_ty, span, ctx)?;
            return Ok((
                self.wrap_value_with_temp_scope(air, view, str_ty, span, prefix)?,
                None,
            ));
        }

        Err(self.type_mismatch_error(str_ty, src_ty, span))
    }

    /// Build the `borrow s: str` view for an operand that names no place
    /// (RUE-953).
    ///
    /// A promoted string operand needs no storage at all: at a `str`
    /// parameter the loan *is* the two-word static-backed view, so the literal
    /// materializes directly under the expected type and is passed by value —
    /// the purest form of the promotion the ruling describes (formal core
    /// §6.13.2: `"hello" : str` is `view⟨A_lit | 0, len⟩` over an allocation in
    /// `H0`).
    ///
    /// Any other operand is evaluated once into a hidden binding that owns it
    /// for the call; the view is then read out of that binding exactly as it
    /// would be from a source place, and the binding's `StorageLive` travels
    /// to the call wrapper so its drop runs after the callee returns.
    fn elaborate_borrow_str_operand(
        &mut self,
        air: &mut Air,
        arg: &RirCallArg,
        str_ty: Type,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<(AirRef, Option<AirRef>)> {
        let span = self.body_rir_ref().get(arg.value).span;

        let prev_expected = ctx.expected_type.replace(str_ty);
        let operand = self.analyze_inst(air, arg.value, ctx);
        ctx.expected_type = prev_expected;
        let operand = operand?;

        // A `str`/`Str(N)` operand already *is* the two-word view: a promoted
        // literal materialized under the expected type, or a view another call
        // produced. There is nothing to own and nothing to drop, so no storage
        // is introduced at all.
        if self.is_str_like(operand.ty) {
            return Ok((operand.air_ref, None));
        }

        // A `StrBuf` operand owns a buffer. The hidden binding owns it for the
        // call, and the view is read out of that binding through the trusted
        // accessors — the same narrowing a source place gets (RUE-1066).
        if self.is_strbuf(operand.ty) {
            let elaborated = self.elaborate_borrow_operand(
                air,
                arg.value,
                operand.air_ref,
                operand.ty,
                span,
                ctx,
            )?;
            let (view, mut prefix) =
                self.strbuf_text_view(air, elaborated.load, operand.ty, span, ctx)?;
            prefix.insert(0, elaborated.alloc);
            let view = self.wrap_value_with_temp_scope(air, view, str_ty, span, prefix)?;
            return Ok((view, Some(elaborated.storage_live)));
        }
        Err(self.type_mismatch_error(str_ty, operand.ty, span))
    }
}
