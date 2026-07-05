//! Linear-type / move / exclusive-access checks and call-argument analysis.
//!
//! Split out of `analysis.rs` (RUE-4); methods are part of the same
//! `impl<'a> Sema<'a>` and behave identically.

use super::*;

impl<'a> Sema<'a> {
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

            // Emit warning with help suggestion (to ctx.warnings for parallel safety)
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

    /// Analyze a list of call arguments, enforcing by-ref argument rules.
    ///
    /// Inout/borrow arguments are borrows, not moves: `ctx.byref_arg_root` is
    /// set to the argument's ROOT variable while the argument value is
    /// analyzed, so the variable-reference and place analyses skip move
    /// tracking for it (and permit forwarding an inout parameter to another
    /// function's by-ref parameter). A by-ref argument must be a place — a
    /// variable, or a field/index projection chain rooted at one (`borrow
    /// o.f`, `inout a[i]`, RUE-143); codegen forms the place's address.
    ///
    /// An `inout` argument rooted at a `borrow` parameter is rejected here:
    /// it would hand the callee a mutable view of read-only memory.
    pub(crate) fn analyze_call_args(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::new();
        for arg in args.iter() {
            let byref_root = if arg.is_inout() || arg.is_borrow() {
                let root = require_byref_place_arg(self.rir, arg)?;
                if arg.is_inout()
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

            // Set while analyzing the argument so the use is treated as a
            // borrow, not a move; restored afterwards.
            let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
            let arg_result = self.analyze_inst(air, arg.value, ctx);
            ctx.byref_arg_root = prev_byref_root;
            let arg_result = arg_result?;

            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(air_args)
    }

    /// Analyze call arguments, materializing a fat-pointer slice value for any
    /// argument whose parameter is a slice type (ADR-0043, RUE-322).
    ///
    /// `borrow arr` (where `arr: [T; N]`) passed to a `borrow s: [T]` parameter
    /// is coerced to a by-value slice `{ptr: @raw(arr[0]), len: N}`, which flows
    /// through the existing by-value aggregate ABI (the parameter is by-value —
    /// see [`crate::sema::Sema`] parameter setup). Non-slice parameters use the
    /// ordinary [`Self::analyze_call_args`] argument path unchanged.
    pub(crate) fn analyze_call_args_coerced(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        param_types: &[Type],
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
            let is_str_param = param_types.get(i).is_some_and(|pt| self.is_str_like(*pt));
            let is_inout_str_param =
                arg.is_inout() && param_types.get(i).is_some_and(|pt| self.is_str_struct(*pt));
            if is_str_param && !is_inout_str_param {
                let str_ty = param_types[i];
                let prev_expected = ctx.expected_type.replace(str_ty);
                let arg_result = self.analyze_inst(air, arg.value, ctx);
                ctx.expected_type = prev_expected;
                let arg_result = arg_result?;
                air_args.push(AirCallArg {
                    value: arg_result.air_ref,
                    mode: AirArgMode::Normal,
                });
                continue;
            }

            let is_slice_param = param_types
                .get(i)
                .is_some_and(|pt| self.slice_element_type(*pt).is_some());
            if is_slice_param && !is_inout_str_param {
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
            let place_ref = air.make_place(trace.base, projs);
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
            let place_ref = air.make_place(trace.base, projs);
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
}
