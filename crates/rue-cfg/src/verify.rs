//! CFG structural verifier (RUE-227).
//!
//! After the CFG is built (and optimized) but before it is lowered to machine
//! code, this pass asserts the structural invariants that codegen relies on.
//! Its whole purpose is to convert a *class* of latent lowering bugs — most
//! notoriously "block has no terminator", which historically surfaced as a
//! `SIGABRT` deep inside `cfg_lower.rs` (RUE-213 / RUE-217 / RUE-224) — into a
//! **loud, early, localized** compiler-bug report naming the function and the
//! offending block.
//!
//! # Why a hard panic (not a `debug_assert`)
//!
//! A CFG that violates these invariants is malformed AIR→CFG output: continuing
//! anyway miscompiles the program (wrong ABI, double-free, silent corruption).
//! Per RUE-45 the guard must fire in **release** builds too, so this uses
//! `panic!`/`assert!`, never `debug_assert!`.
//!
//! # Invariants checked (for every *reachable* block)
//!
//! 1. **Termination** — the block ends in a real terminator (`Goto`, `Branch`,
//!    `Switch`, `Return`, or `Unreachable`), never the `None` placeholder that a
//!    mid-block `diverged()` bail leaves behind.
//! 2. **Value definition** — every `CfgValue` referenced by an instruction or a
//!    terminator indexes a value that actually exists in the CFG.
//! 3. **Block-param arity** — the number of block-arguments passed along each
//!    edge equals the arity of the successor block's parameter list.
//! 4. **Block-param types** (RUE-347) — each block-argument's type equals the
//!    corresponding successor parameter's type. This is the block-parameter
//!    type invariant SSA passes and codegen rely on; a divergence-handling bug
//!    in lowering (e.g. wiring a unit "result" of a `!`-typed arm into a join)
//!    trips it here instead of miscompiling later.
//!
//! Unreachable blocks skip the termination check only: an orphan block created
//! but never wired in (or one left behind before DCE) may legitimately carry a
//! `None` terminator, and it never reaches codegen. But an unreachable block
//! that *does* carry a terminator still has its edges arity- and type-checked
//! (invariants 3 and 4): such blocks are exactly where divergence-handling
//! bugs park ill-typed edges "harmlessly" (RUE-347), and a pass or backend
//! change that consults edge types before checking reachability would turn
//! one into a real miscompile.

use crate::inst::{Cfg, CfgInstData, CfgValue, Projection, Terminator};

impl Cfg {
    /// Verify the CFG's structural invariants, panicking with a precise,
    /// function- and block-localized message on any violation.
    ///
    /// See the module docs for the invariants and the rationale for the hard
    /// panic. This is a compiler-bug guard: a well-formed pipeline never trips
    /// it, so the cost (one linear walk of reachable blocks) is paid only to
    /// pin future lowering regressions to their source.
    pub fn verify(&self) {
        let reachable = self.reachable_blocks();
        let value_count = self.value_count() as u32;

        for block in self.blocks() {
            if !reachable[block.id.as_u32() as usize] {
                // Unreachable blocks may legitimately be unterminated (module
                // docs), but if one carries a real terminator its edges must
                // still be well-formed: arity- and type-check them (RUE-347).
                if !matches!(block.terminator, Terminator::None) {
                    self.verify_terminator(block.id, &block.terminator, value_count);
                }
                continue;
            }

            // (1) Termination.
            if matches!(block.terminator, Terminator::None) {
                panic!(
                    "internal compiler error: CFG verification failed in function \
                     `{}`: reachable block {} has no terminator (an AIR→CFG lowering \
                     path left the block unterminated — likely an expression used in \
                     value position that produced no CFG value). This is a compiler \
                     bug (RUE-227).",
                    self.fn_name(),
                    block.id,
                );
            }

            // (2) Value definition — block params and instruction operands.
            for &(param_val, _) in &block.params {
                self.check_value_defined(param_val, value_count, block.id, "block parameter");
            }
            for &inst_val in &block.insts {
                self.check_value_defined(inst_val, value_count, block.id, "instruction result");
                let mut operands = Vec::new();
                self.collect_inst_operands(&self.get_inst(inst_val).data, &mut operands);
                for op in operands {
                    self.check_value_defined(op, value_count, block.id, "instruction operand");
                }
            }

            // (2) + (3) Terminator operands and edge arities.
            self.verify_terminator(block.id, &block.terminator, value_count);
        }
    }

    /// Panic if `value` does not index a defined value in this CFG.
    #[inline]
    fn check_value_defined(
        &self,
        value: CfgValue,
        value_count: u32,
        block: crate::inst::BlockId,
        role: &str,
    ) {
        assert!(
            value.as_u32() < value_count,
            "internal compiler error: CFG verification failed in function `{}`: {} \
             {} in block {} references an undefined value (only {} values exist). \
             This is a compiler bug (RUE-227).",
            self.fn_name(),
            role,
            value,
            block,
            value_count,
        );
    }

    /// Verify a block's terminator: operand definedness and successor arities.
    fn verify_terminator(&self, block: crate::inst::BlockId, term: &Terminator, value_count: u32) {
        match term {
            Terminator::Goto {
                target,
                args_start,
                args_len,
            } => {
                let args = self.get_extra(*args_start, *args_len);
                for &arg in args {
                    self.check_value_defined(arg, value_count, block, "goto argument");
                }
                self.check_edge_args(block, *target, args);
            }
            Terminator::Branch {
                cond,
                then_block,
                then_args_start,
                then_args_len,
                else_block,
                else_args_start,
                else_args_len,
            } => {
                self.check_value_defined(*cond, value_count, block, "branch condition");
                let then_args = self.get_extra(*then_args_start, *then_args_len);
                for &arg in then_args {
                    self.check_value_defined(arg, value_count, block, "branch-then argument");
                }
                self.check_edge_args(block, *then_block, then_args);
                let else_args = self.get_extra(*else_args_start, *else_args_len);
                for &arg in else_args {
                    self.check_value_defined(arg, value_count, block, "branch-else argument");
                }
                self.check_edge_args(block, *else_block, else_args);
            }
            Terminator::Switch {
                scrutinee,
                cases_start,
                cases_len,
                default,
            } => {
                self.check_value_defined(*scrutinee, value_count, block, "switch scrutinee");
                // Switch edges carry no block-arguments, so every successor
                // must have an empty parameter list.
                for (_, target) in self.get_switch_cases(*cases_start, *cases_len) {
                    self.check_edge_args(block, *target, &[]);
                }
                self.check_edge_args(block, *default, &[]);
            }
            Terminator::Return { value } => {
                if let Some(v) = value {
                    self.check_value_defined(*v, value_count, block, "return value");
                }
            }
            Terminator::Unreachable | Terminator::None => {}
        }
    }

    /// Panic if the block-arguments on an edge do not match the successor
    /// block's parameter list — in count (RUE-227) or, position by position,
    /// in type (RUE-347).
    fn check_edge_args(
        &self,
        from: crate::inst::BlockId,
        to: crate::inst::BlockId,
        args: &[CfgValue],
    ) {
        let params = &self.get_block(to).params;
        assert!(
            params.len() == args.len(),
            "internal compiler error: CFG verification failed in function `{}`: edge \
             {} -> {} passes {} block-argument(s) but the target expects {} block \
             parameter(s). This is a compiler bug (RUE-227).",
            self.fn_name(),
            from,
            to,
            args.len(),
            params.len(),
        );
        for (i, (&arg, &(_, param_ty))) in args.iter().zip(params.iter()).enumerate() {
            let arg_ty = self.get_inst(arg).ty;
            assert!(
                arg_ty == param_ty,
                "internal compiler error: CFG verification failed in function `{}`: edge \
                 {} -> {} passes block-argument {} ({}) with type {:?} but the target \
                 parameter has type {:?} — an ill-typed edge, usually a divergence-handling \
                 bug in lowering. This is a compiler bug (RUE-347).",
                self.fn_name(),
                from,
                to,
                i,
                arg,
                arg_ty,
                param_ty,
            );
        }
    }

    /// Compute which blocks are reachable from the entry block by following
    /// terminator successors.
    fn reachable_blocks(&self) -> Vec<bool> {
        let mut reachable = vec![false; self.block_count()];
        if self.block_count() == 0 {
            return reachable;
        }
        let mut stack = vec![self.entry];
        reachable[self.entry.as_u32() as usize] = true;
        while let Some(id) = stack.pop() {
            let mut push = |succ: crate::inst::BlockId| {
                let idx = succ.as_u32() as usize;
                if !reachable[idx] {
                    reachable[idx] = true;
                    stack.push(succ);
                }
            };
            match &self.get_block(id).terminator {
                Terminator::Goto { target, .. } => push(*target),
                Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    push(*then_block);
                    push(*else_block);
                }
                Terminator::Switch {
                    cases_start,
                    cases_len,
                    default,
                    ..
                } => {
                    for (_, target) in self.get_switch_cases(*cases_start, *cases_len) {
                        push(*target);
                    }
                    push(*default);
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
        }
        reachable
    }

    /// Collect every `CfgValue` operand referenced by an instruction into
    /// `out`. This mirrors the `CfgInstData` variants; a new variant with value
    /// operands must be added here so the verifier keeps seeing all references.
    fn collect_inst_operands(&self, data: &CfgInstData, out: &mut Vec<CfgValue>) {
        match data {
            // No value operands.
            CfgInstData::Const(_)
            | CfgInstData::BoolConst(_)
            | CfgInstData::StringConst(_)
            | CfgInstData::Param { .. }
            | CfgInstData::BlockParam { .. }
            | CfgInstData::Load { .. }
            | CfgInstData::StorageLive { .. }
            | CfgInstData::StorageDead { .. } => {}

            // Binary operations.
            CfgInstData::Add(a, b)
            | CfgInstData::Sub(a, b)
            | CfgInstData::Mul(a, b)
            | CfgInstData::Div(a, b)
            | CfgInstData::Mod(a, b)
            | CfgInstData::Eq(a, b)
            | CfgInstData::Ne(a, b)
            | CfgInstData::Lt(a, b)
            | CfgInstData::Gt(a, b)
            | CfgInstData::Le(a, b)
            | CfgInstData::Ge(a, b)
            | CfgInstData::BitAnd(a, b)
            | CfgInstData::BitOr(a, b)
            | CfgInstData::BitXor(a, b)
            | CfgInstData::Shl(a, b)
            | CfgInstData::Shr(a, b) => {
                out.push(*a);
                out.push(*b);
            }

            // Unary operations.
            CfgInstData::Neg(v) | CfgInstData::Not(v) | CfgInstData::BitNot(v) => out.push(*v),

            CfgInstData::Alloc { init, .. } => out.push(*init),
            CfgInstData::Store { value, .. } => out.push(*value),
            CfgInstData::ParamStore { value, .. } => out.push(*value),

            CfgInstData::PlaceRead { place } => self.collect_place_operands(place, out),
            CfgInstData::PlaceWrite { place, value } => {
                self.collect_place_operands(place, out);
                out.push(*value);
            }

            CfgInstData::Call {
                args_start,
                args_len,
                ..
            } => {
                for arg in self.get_call_args(*args_start, *args_len) {
                    out.push(arg.value);
                }
            }
            CfgInstData::Intrinsic {
                args_start,
                args_len,
                ..
            } => out.extend_from_slice(self.get_extra(*args_start, *args_len)),

            CfgInstData::StructInit {
                fields_start,
                fields_len,
                ..
            } => out.extend_from_slice(self.get_extra(*fields_start, *fields_len)),
            CfgInstData::FieldSet { value, .. } => out.push(*value),
            CfgInstData::ParamFieldSet { value, .. } => out.push(*value),

            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => out.extend_from_slice(self.get_extra(*elements_start, *elements_len)),
            CfgInstData::IndexSet { index, value, .. } => {
                out.push(*index);
                out.push(*value);
            }
            CfgInstData::ParamIndexSet { index, value, .. } => {
                out.push(*index);
                out.push(*value);
            }

            CfgInstData::EnumVariant {
                payload_start,
                payload_len,
                ..
            } => out.extend_from_slice(self.get_extra(*payload_start, *payload_len)),
            CfgInstData::EnumPayloadGet { base, .. } => out.push(*base),

            CfgInstData::IntCast { value, .. } => out.push(*value),
            CfgInstData::Drop { value } => out.push(*value),
        }
    }

    /// Collect the `CfgValue` operands hiding inside a place's `Index`
    /// projections.
    fn collect_place_operands(&self, place: &crate::inst::Place, out: &mut Vec<CfgValue>) {
        for proj in self.get_place_projections(place) {
            if let Projection::Index { index, .. } = proj {
                out.push(*index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::inst::{Cfg, CfgInst, CfgInstData, Terminator};
    use rue_air::Type;
    use rue_span::Span;

    fn unit_cfg() -> Cfg {
        Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![])
    }

    #[test]
    fn verify_accepts_terminated_block() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let v = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 2),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: Some(v) });
        cfg.verify(); // must not panic
    }

    #[test]
    #[should_panic(expected = "has no terminator")]
    fn verify_catches_missing_terminator() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(0, 1),
            },
        );
        // Deliberately leave the reachable entry block with Terminator::None.
        cfg.verify();
    }

    #[test]
    fn verify_ignores_unreachable_unterminated_block() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, Terminator::Return { value: None });
        // An orphan block, never wired in, with no terminator: must be skipped.
        let _orphan = cfg.new_block();
        cfg.verify();
    }

    #[test]
    #[should_panic(expected = "block-argument")]
    fn verify_catches_arity_mismatch() {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        // Target expects one block parameter...
        cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: None });
        // ...but the goto edge passes zero arguments.
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target,
                args_start: 0,
                args_len: 0,
            },
        );
        cfg.verify();
    }

    /// Build `entry --goto(one arg of `arg_ty`)--> target(param: i32)`.
    fn cfg_with_typed_edge(arg_ty: Type) -> Cfg {
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        let target = cfg.new_block();
        cfg.add_block_param(target, Type::I32);
        cfg.set_terminator(target, Terminator::Return { value: None });
        let arg = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: arg_ty,
                span: Span::new(0, 1),
            },
        );
        let (args_start, args_len) = cfg.push_extra(vec![arg]);
        cfg.set_terminator(
            entry,
            Terminator::Goto {
                target,
                args_start,
                args_len,
            },
        );
        cfg
    }

    #[test]
    fn verify_accepts_well_typed_edge() {
        cfg_with_typed_edge(Type::I32).verify(); // must not panic
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch() {
        // The RUE-347 shape: a unit value passed into an i32 block parameter.
        cfg_with_typed_edge(Type::UNIT).verify();
    }

    #[test]
    #[should_panic(expected = "ill-typed edge")]
    fn verify_catches_edge_type_mismatch_in_unreachable_block() {
        // Ill-typed edges parked in unreachable blocks are exactly where
        // divergence-handling bugs hide (RUE-347) — they must still be caught.
        let mut cfg = unit_cfg();
        let entry = cfg.new_block();
        cfg.entry = entry;
        cfg.set_terminator(entry, Terminator::Return { value: None });
        // Unreachable-but-terminated pair with a unit→i32 edge.
        let orphan_from = cfg.new_block();
        let orphan_to = cfg.new_block();
        cfg.add_block_param(orphan_to, Type::I32);
        cfg.set_terminator(orphan_to, Terminator::Return { value: None });
        let arg = cfg.add_inst_to_block(
            orphan_from,
            CfgInst {
                data: CfgInstData::Const(0),
                ty: Type::UNIT,
                span: Span::new(0, 1),
            },
        );
        let (args_start, args_len) = cfg.push_extra(vec![arg]);
        cfg.set_terminator(
            orphan_from,
            Terminator::Goto {
                target: orphan_to,
                args_start,
                args_len,
            },
        );
        cfg.verify();
    }
}
