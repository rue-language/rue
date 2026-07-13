//! Linear-type / move / exclusive-access checks and call-argument analysis.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

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

impl<'a> BodySema<'a> {
    /// Check if directives contain @allow for a specific warning name.
    pub(crate) fn has_allow_directive(
        &self,
        directives: &[RirDirective],
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.interner.get("allow");
        let warning_sym = self.interner.get(warning_name);

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
            let name = self.interner.resolve(&*symbol);

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

            // Only check types that require consumption: linear structs
            // themselves, and non-empty arrays whose elements carry one (an
            // array of linear values must be consumed — as a whole, or
            // element-wise via constant-index moves (RUE-186); dropping it
            // would silently drop every element).
            if !self.type_requires_consumption(local.ty) {
                continue;
            }

            // Consumption must hold on EVERY path (must-consume).
            // `full_move` alone is may-move (union at branch joins): a value
            // consumed in only one branch of an if/match still leaks on the
            // other paths.
            let state = ctx.moved_vars.get(symbol);
            if state.is_some_and(|s| s.full_move_on_all_paths) {
                continue;
            }

            // Element-wise consumption of a linear array (RUE-186, spec
            // 3.8:71): moving every element out (constant indices) on every
            // path satisfies the array's must-consume obligation. Partial
            // element consumption is an error naming the missing elements.
            match self.check_array_elementwise_consumption(local.ty, state, *symbol, local.span)? {
                ElementwiseConsumption::Complete => continue,
                ElementwiseConsumption::NotElementwise => {}
            }

            let name = self.interner.resolve(&*symbol);
            let err = linear_not_consumed_error(name, local.span, state.and_then(|s| s.full_move));
            return Err(self.attach_infectious_linear_note(err, local.ty));
        }

        Ok(())
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
                let (element_type, length) = self.type_pool.array_def(array_id);
                length > 0 && self.type_requires_consumption(element_type)
            }
            _ => self.type_carries_linear(ty),
        }
    }

    /// Shared element-wise consumption check for linear arrays (RUE-186):
    /// returns `Complete` when every element of the array was moved out on
    /// every path (the must-consume obligation is satisfied), an `Err`
    /// naming the missing elements when the array was only PARTIALLY
    /// consumed element-wise, and `NotElementwise` when no element was ever
    /// consumed (or the type is not an array) — the caller then reports its
    /// usual whole-value diagnostic.
    pub(super) fn check_array_elementwise_consumption(
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
        let (_elem, len) = self.type_pool.array_def(array_id);
        let elem_path = |k: u64| vec![index_path_segment(self.interner, k)];
        let unconsumed: Vec<u64> = (0..len)
            .filter(|k| !s.partial_moves_on_all_paths.contains(&elem_path(*k)))
            .collect();
        if unconsumed.is_empty() {
            return Ok(ElementwiseConsumption::Complete);
        }
        let touched_any_element = s.partial_moves.iter().any(|(p, _)| {
            p.first()
                .is_some_and(|seg| is_index_segment(self.interner, *seg))
        });
        if !touched_any_element {
            return Ok(ElementwiseConsumption::NotElementwise);
        }
        let name = self.interner.resolve(&symbol);
        // An unconsumed element that WAS moved on some path selects the
        // more precise "not consumed on all paths" diagnostic (E0443 over
        // E0406).
        let some_path_span = unconsumed.iter().find_map(|k| {
            let target = elem_path(*k);
            s.partial_moves
                .iter()
                .find(|(p, _)| *p == target)
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
        let type_name = dest_ty.safe_name_with_pool(Some(&self.type_pool));
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
        span: Span,
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
        // live element to drop. Reuse the must-consume element check; `Err`
        // (partial consumption) and `NotElementwise` are both "not discharged".
        assigned_path.is_empty()
            && matches!(
                self.check_array_elementwise_consumption(dest_ty, Some(state), root_var, span),
                Ok(ElementwiseConsumption::Complete)
            )
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
                type_name: ty.safe_name_with_pool(Some(&self.type_pool)),
            },
            self.rir.get(inst_ref).span,
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
        match self.infectious_linear.get(&struct_id) {
            Some((field_name, field_type)) => err.with_note(format!(
                "'{}' is linear because field '{}' of type '{}' carries a linear value",
                self.type_pool.struct_def(struct_id).name,
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
    pub(crate) fn record_element_move_out(
        &mut self,
        trace: &super::super::analyze_ops::PlaceTrace,
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
        let elem_path = vec![index_path_segment(self.interner, k as u64)];
        if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
            // Whole-element move: reject if the element itself, an ancestor, or
            // any descendant subfield was already moved (`arr[0]` can't be
            // moved once `arr[0].s` moved — spec 3.8, RUE-279).
            if let Some(moved_span) = state.is_path_or_descendant_moved(&elem_path) {
                return Err(use_after_move_path_error(
                    self.interner,
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
        Ok(Some(k))
    }

    /// Export a recorded element move (RUE-186) to drop elaboration: wrap
    /// `value` in a [`AirInstData::MarkMoved`] whose place carries a
    /// constant-index projection (the CFG builder resolves it back to the
    /// element index; see `moved_field_path` in `rue-cfg`). Only bases that
    /// drop elaboration would drop get a marker: locals, and Normal (owned)
    /// params — mirroring the field-move marker conditions.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit_element_move_marker(
        &self,
        air: &mut Air,
        trace: &super::super::analyze_ops::PlaceTrace,
        ctx: &AnalysisContext,
        value: AirRef,
        k: i64,
        elem_type: Type,
        array_type: Type,
        span: Span,
    ) -> AirRef {
        let droppable_base = match trace.base {
            AirPlaceBase::Local(_) => true,
            AirPlaceBase::Param(_) => ctx
                .params
                .iter()
                .any(|p| p.name == trace.root_var && p.mode == RirParamMode::Normal),
        };
        if !droppable_base {
            return value;
        }
        let (slot, is_param) = match trace.base {
            AirPlaceBase::Local(slot) => (slot, false),
            AirPlaceBase::Param(slot) => (slot, true),
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
        );
        air.add_inst(AirInst {
            data: AirInstData::MarkMoved {
                value,
                slot,
                is_param,
                place: Some(marker_place),
            },
            ty: elem_type,
            span,
        })
    }

    /// Reject a read through an array element that is (or may be) moved out
    /// (RUE-186). Element moves only exist for indices rooted directly at an
    /// array variable, so only a position-0 `Index` projection can read
    /// through one. A constant index checks exactly that element's path; a
    /// dynamic index conservatively fails on ANY outstanding partial move of
    /// the root (sema can't know which element is read — soundness).
    pub(crate) fn check_read_through_moved_element(
        &self,
        trace: &super::super::analyze_ops::PlaceTrace,
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
                let elem_path = vec![index_path_segment(self.interner, k as u64)];
                if let Some(moved_span) = state.is_path_moved(&elem_path) {
                    return Err(use_after_move_path_error(
                        self.interner,
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
                        self.interner,
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
    /// is not supported — sema-side reinitialization and the runtime drop
    /// flags would disagree — so the whole array must be reinitialized
    /// instead. Writes into arrays with no outstanding element moves are
    /// unaffected.
    pub(crate) fn reject_write_into_partially_moved_array(
        &self,
        trace: &super::super::analyze_ops::PlaceTrace,
        ctx: &AnalysisContext,
        span: Span,
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
            p.first()
                .filter(|seg| is_index_segment(self.interner, **seg))
                .map(|_| *s)
        });
        let Some(moved_span) = element_move_span else {
            return Ok(());
        };
        let name = self.interner.resolve(&trace.root_var).to_string();
        Err(
            CompileError::new(ErrorKind::AssignToPartiallyMovedArray { array: name }, span)
                .with_label("element moved out here", moved_span)
                .with_help("reinitialize the whole array instead (`xs = [...]`)"),
        )
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
        root_variable_of(self.rir, inst_ref)
    }

    /// Whether a method receiver rooted at `root` binds a mutable place, for
    /// the `inout self` mut-binding requirement (RUE-15). A `let mut` local is
    /// mutable; an `inout` parameter is mutable (it already names mutable
    /// caller storage); everything else (`let`, `borrow` params) is not.
    /// Mirrors `PlaceTrace::is_root_mutable`.
    pub(super) fn receiver_root_is_mutable(&self, root: Spur, ctx: &AnalysisContext) -> bool {
        // Locals shadow parameters (RUE-278): a `let mut` rebinding a param
        // name is the mutable binding that later receiver uses see.
        if let Some(local) = ctx.locals.get(&root) {
            return local.is_mut;
        }
        if let Some(param) = ctx.params.iter().find(|p| p.name == root) {
            return param.mode == RirParamMode::Inout;
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
                    variable: self.interner.resolve(&root_var).to_string(),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Check exclusivity rules for inout and borrow parameters in a call
    /// (adapter over the shared [`check_exclusive_access_in`], RUE-141).
    pub(crate) fn check_exclusive_access(
        &self,
        args: &[RirCallArg],
        call_span: Span,
    ) -> CompileResult<()> {
        check_exclusive_access_in(self.rir, self.interner, args, call_span)
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
                let variable = self.interner.resolve(&root).to_string();
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
        Ok(())
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
        if let InstData::VarRef { name } = self.rir.get(operand).data {
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
        if self.is_builtin_string(found) || self.is_str_fixed_struct(found) {
            let found_name = found.safe_name_with_pool(Some(&self.type_pool));
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
        if self.is_builtin_string(found)
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
            match self.rir.get(current).data {
                InstData::Block { extra_start, len } if len > 0 => {
                    let refs = self.rir.get_extra(extra_start, len);
                    current = InstRef::from_raw(refs[len as usize - 1]);
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
    /// see [`crate::sema::Sema`] parameter setup). All other arguments retain
    /// the ordinary by-value/by-reference analysis in this same chokepoint.
    pub(crate) fn analyze_call_args_coerced(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        param_types: &[Type],
        param_modes: &[RirParamMode],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        // Loan-frame discipline (RUE-523): a by-value move of a root this call
        // passes `inout`/`borrow` conflicts in either argument order.
        let frame: Vec<(Spur, CallLoanKind)> = args
            .iter()
            .filter_map(|arg| {
                let kind = if arg.is_inout() {
                    CallLoanKind::Inout
                } else if arg.is_borrow() {
                    CallLoanKind::Borrow
                } else {
                    return None;
                };
                root_variable_of(self.rir, arg.value).map(|root| (root, kind))
            })
            .collect();
        let pushed = !frame.is_empty();
        if pushed {
            ctx.call_loaned_roots.push(frame);
        }
        let result = self.analyze_call_args_coerced_inner(air, args, param_types, param_modes, ctx);
        if pushed {
            ctx.call_loaned_roots.pop();
        }
        result
    }

    /// The argument loop behind [`Sema::analyze_call_args_coerced`], factored
    /// out so the loan frame is popped on every exit path.
    fn analyze_call_args_coerced_inner(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        param_types: &[Type],
        param_modes: &[RirParamMode],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
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
                // The argument is always a place: `check_exclusive_access`
                // rejected non-lvalue `borrow` arguments (E0427) before
                // argument analysis began.
                if arg.is_borrow() && self.is_str_struct(str_ty) {
                    let value = self.coerce_borrow_str_place_to_view(air, arg, str_ty, ctx)?;
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
                        self.rir.get(arg.value).span,
                    ));
                }
                // Two-types model (ADR-0043, RUE-386): a bare `str` parameter
                // (Normal mode) requires a first-class `str`; a buffer or a
                // borrowed view passed here would escape and dangle. A
                // `borrow str` parameter (Borrow mode) is the sanctioned view
                // and accepts a buffer, so it is exempt.
                if matches!(param_mode, RirParamMode::Normal | RirParamMode::Comptime)
                    && self.is_str_struct(str_ty)
                {
                    self.reject_non_first_class_str(
                        arg.value,
                        arg_result.ty,
                        FirstClassStrSite::Param,
                        self.rir.get(arg.value).span,
                        ctx,
                    )?;
                    if !arg_result.ty.can_coerce_to(&str_ty) {
                        return Err(self.type_mismatch_error(
                            str_ty,
                            arg_result.ty,
                            self.rir.get(arg.value).span,
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
                let value = self.coerce_borrow_array_to_slice(air, arg, slice_ty, ctx)?;
                air_args.push(AirCallArg {
                    value,
                    // The fat pointer is passed BY VALUE (multi-slot aggregate).
                    mode: AirArgMode::Normal,
                });
                continue;
            }

            let byref_root = if arg.is_inout() || arg.is_borrow() {
                let root = require_byref_place_arg(self.rir, arg)?;
                if arg.is_inout()
                    && !ctx.locals.contains_key(&root)
                    && ctx
                        .params
                        .iter()
                        .any(|p| p.name == root && p.mode == RirParamMode::Borrow)
                {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: self.interner.resolve(&root).to_string(),
                        },
                        self.rir.get(arg.value).span,
                    ));
                }
                Some(root)
            } else {
                None
            };
            let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
            let arg_result = self.analyze_inst(air, arg.value, ctx);
            ctx.byref_arg_root = prev_byref_root;
            let arg_result = arg_result?;
            // Two-types model (ADR-0043, RUE-386): an `inout str` parameter is
            // an *exclusive* view and requires local provenance — a first-class
            // / static `str` value is never a legal exclusive operand (closes
            // the write-to-`.rodata` and Copy-two-roots aliasing holes).
            if is_inout_str_param {
                self.validate_inout_str_operand(
                    arg.value,
                    param_ty,
                    arg_result.ty,
                    self.rir.get(arg.value).span,
                    ctx,
                )?;
            }
            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(air_args)
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

        let span = self.rir.get(arg.value).span;
        let slice_struct_id = slice_ty
            .as_struct()
            .expect("slice type is a synthetic struct");
        let slice_elem = self
            .slice_element_type(slice_ty)
            .expect("slice type has an element");
        let ptr_ty = self.type_pool.struct_def(slice_struct_id).fields[0].ty;

        // A slice argument must be `borrow arr` (the shared form; `inout` slices
        // are a later phase). The array is borrowed (address taken), not moved.
        if !arg.is_borrow() {
            return Err(CompileError::new(ErrorKind::BorrowKeywordMissing, span));
        }
        let root = require_byref_place_arg(self.rir, arg)?;
        let prev_byref_root = ctx.byref_arg_root.replace(root);
        let trace = self.try_trace_place(arg.value, air, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let trace = trace?.ok_or_else(|| CompileError::new(ErrorKind::BorrowNonLvalue, span))?;

        let arr_ty = trace.result_type();

        // Forwarding an existing slice (`inner(borrow s)` where `s: [T]`): the
        // fat pointer is already built, so read it by value (a slice is `@copy`)
        // and pass it through — no re-materialization.
        if arr_ty == slice_ty {
            let projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            let place_ref = air.make_place(trace.base, trace.base_type, projs);
            let val = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: slice_ty,
                span,
            });
            return Ok(val);
        }

        let (arr_elem, arr_len) = match arr_ty.as_array() {
            Some(id) => self.type_pool.array_def(id),
            None => {
                // Only whole-array borrow-to-slice is supported in Phase 1.
                return Err(self.type_mismatch_error(slice_ty, arr_ty, span));
            }
        };
        if arr_elem != slice_elem {
            return Err(self.type_mismatch_error(slice_ty, arr_ty, span));
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
            let ptr_args = air.add_extra(&[zero_ref.as_u32()]);
            air.add_inst(AirInst {
                data: AirInstData::Intrinsic {
                    name: self.known.int_to_ptr,
                    args_start: ptr_args,
                    args_len: 1,
                },
                ty: ptr_ty,
                span,
            })
        } else {
            // ptr word = @raw(arr[0]) : ptr const T. Build a place read of
            // element 0 and take its address, exactly as source `@raw(arr[0])`
            // would.
            let mut projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            projs.push(AirProjection::Index {
                array_type: arr_ty,
                index: zero_ref,
            });
            let place_ref = air.make_place(trace.base, trace.base_type, projs);
            let elem0_read = air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: arr_elem,
                span,
            });
            let raw_args = air.add_extra(&[elem0_read.as_u32()]);
            air.add_inst(AirInst {
                data: AirInstData::Intrinsic {
                    name: self.known.raw,
                    args_start: raw_args,
                    args_len: 1,
                },
                ty: ptr_ty,
                span,
            })
        };

        // len word = N (compile-time array length).
        let len_ref = air.add_inst(AirInst {
            data: AirInstData::Const(arr_len),
            ty: Type::U64,
            span,
        });

        // Materialize the fat pointer `{ptr, len}` struct value.
        let fields = air.add_extra(&[ptr_ref.as_u32(), len_ref.as_u32()]);
        let source_order = air.add_extra(&[0u32, 1u32]);
        let slice_val = air.add_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id: slice_struct_id,
                fields_start: fields,
                fields_len: 2,
                source_order_start: source_order,
            },
            ty: slice_ty,
            span,
        });
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
    fn coerce_borrow_str_place_to_view(
        &mut self,
        air: &mut Air,
        arg: &RirCallArg,
        str_ty: Type,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AirRef> {
        use crate::inst::{AirInstData, AirProjection};

        let span = self.rir.get(arg.value).span;
        let root = require_byref_place_arg(self.rir, arg)?;
        let prev_byref_root = ctx.byref_arg_root.replace(root);
        let trace = self.try_trace_place(arg.value, air, ctx);
        ctx.byref_arg_root = prev_byref_root;
        let trace = trace?.ok_or_else(|| CompileError::new(ErrorKind::BorrowNonLvalue, span))?;

        let src_ty = trace.result_type();

        // `str` / `Str(N)` sources share the 2-word `{ptr, len}` shape: read
        // the fat pointer by value (it is `@copy`) and pass it through.
        if self.is_str_like(src_ty) {
            let projs: Vec<AirProjection> = trace.projections.iter().map(|p| p.proj).collect();
            let place_ref = air.make_place(trace.base, trace.base_type, projs);
            return Ok(air.add_inst(AirInst {
                data: AirInstData::PlaceRead { place: place_ref },
                ty: str_ty,
                span,
            }));
        }

        // `StrBuf` source: the view is the `{ptr, len}` prefix of the 3-word
        // buffer header, read field-by-field from the borrowed place.
        if self.is_builtin_string(src_ty) {
            let buf_struct_id = src_ty.as_struct().expect("StrBuf is a synthetic struct");
            let str_struct_id = str_ty.as_struct().expect("str is a synthetic struct");
            let mut field_words = Vec::with_capacity(2);
            for field_index in 0..2u32 {
                let mut projs: Vec<AirProjection> =
                    trace.projections.iter().map(|p| p.proj).collect();
                projs.push(AirProjection::Field {
                    struct_id: buf_struct_id,
                    field_index,
                });
                let place_ref = air.make_place(trace.base, trace.base_type, projs);
                let field_ty =
                    self.type_pool.struct_def(buf_struct_id).fields[field_index as usize].ty;
                let word = air.add_inst(AirInst {
                    data: AirInstData::PlaceRead { place: place_ref },
                    ty: field_ty,
                    span,
                });
                field_words.push(word.as_u32());
            }
            let fields = air.add_extra(&field_words);
            let source_order = air.add_extra(&[0u32, 1u32]);
            return Ok(air.add_inst(AirInst {
                data: AirInstData::StructInit {
                    struct_id: str_struct_id,
                    fields_start: fields,
                    fields_len: 2,
                    source_order_start: source_order,
                },
                ty: str_ty,
                span,
            }));
        }

        Err(self.type_mismatch_error(str_ty, src_ty, span))
    }
}
