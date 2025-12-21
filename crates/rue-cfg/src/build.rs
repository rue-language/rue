//! AIR to CFG lowering.
//!
//! This module converts the structured control flow in AIR (Branch, Loop)
//! into explicit basic blocks with terminators.

use rue_air::{Air, AirInstData, AirPattern, AirRef, Type};
use rue_error::{CompileWarning, WarningKind};

use crate::CfgOutput;
use crate::inst::{BlockId, Cfg, CfgInst, CfgInstData, CfgValue, Terminator};

/// Result of lowering an expression.
struct ExprResult {
    /// The value produced (if any - statements like Store don't produce values)
    value: Option<CfgValue>,
    /// Whether control flow continues after this expression
    continuation: Continuation,
}

/// How control flow continues after an expression.
enum Continuation {
    /// Control continues normally (can add more instructions)
    Continues,
    /// Control flow diverged (return, break, continue) - no more instructions
    Diverged,
}

/// Loop context for break/continue handling.
struct LoopContext {
    /// Block to jump to for continue (loop header)
    header: BlockId,
    /// Block to jump to for break (loop exit)
    exit: BlockId,
}

/// Builder that converts AIR to CFG.
pub struct CfgBuilder<'a> {
    air: &'a Air,
    cfg: Cfg,
    /// Current block we're building
    current_block: BlockId,
    /// Stack of loop contexts for nested loops
    loop_stack: Vec<LoopContext>,
    /// Cache: maps AIR refs to CFG values (for already-lowered instructions)
    value_cache: Vec<Option<CfgValue>>,
    /// Warnings collected during CFG construction (e.g., unreachable code)
    warnings: Vec<CompileWarning>,
}

impl<'a> CfgBuilder<'a> {
    /// Build a CFG from AIR, returning the CFG and any warnings.
    pub fn build(air: &'a Air, num_locals: u32, num_params: u32, fn_name: &str) -> CfgOutput {
        let mut builder = CfgBuilder {
            air,
            cfg: Cfg::new(
                air.return_type(),
                num_locals,
                num_params,
                fn_name.to_string(),
            ),
            current_block: BlockId(0),
            loop_stack: Vec::new(),
            value_cache: vec![None; air.len()],
            warnings: Vec::new(),
        };

        // Create entry block
        builder.current_block = builder.cfg.new_block();
        builder.cfg.entry = builder.current_block;

        // Find the root (should be Ret as last instruction)
        if air.len() > 0 {
            let root = AirRef::from_raw((air.len() - 1) as u32);
            builder.lower_inst(root);
        }

        // Compute predecessor lists
        builder.cfg.compute_predecessors();

        CfgOutput {
            cfg: builder.cfg,
            warnings: builder.warnings,
        }
    }

    /// Lower an AIR instruction, returning its result.
    fn lower_inst(&mut self, air_ref: AirRef) -> ExprResult {
        // Check cache first
        if let Some(cached) = self.value_cache[air_ref.as_u32() as usize] {
            return ExprResult {
                value: Some(cached),
                continuation: Continuation::Continues,
            };
        }

        let inst = self.air.get(air_ref);
        let span = inst.span;
        let ty = inst.ty;

        match &inst.data.clone() {
            AirInstData::Const(v) => {
                let value = self.emit(CfgInstData::Const(*v), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::BoolConst(v) => {
                let value = self.emit(CfgInstData::BoolConst(*v), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::UnitConst => {
                // Unit constants have no runtime representation.
                // We emit a dummy const 0 with unit type for uniformity,
                // but codegen will ignore values of unit type.
                let value = self.emit(CfgInstData::Const(0), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Param { index } => {
                let value = self.emit(CfgInstData::Param { index: *index }, ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Add(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Add(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Sub(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Sub(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Mul(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Mul(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Div(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Div(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Mod(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Mod(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Eq(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Eq(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Ne(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Ne(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Lt(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Lt(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Gt(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Gt(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Le(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Le(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Ge(lhs, rhs) => {
                let lhs_val = self.lower_inst(*lhs).value.unwrap();
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                let value = self.emit(CfgInstData::Ge(lhs_val, rhs_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::And(lhs, rhs) => {
                // Short-circuit: if lhs is false, result is false
                // We need to create blocks for this
                let lhs_val = self.lower_inst(*lhs).value.unwrap();

                let rhs_block = self.cfg.new_block();
                let join_block = self.cfg.new_block();

                // Add block parameter for the result
                let result_param = self.cfg.add_block_param(join_block, Type::Bool);

                // Branch: if lhs is false, go to join with false; else evaluate rhs
                let false_val = self.emit(CfgInstData::BoolConst(false), Type::Bool, span);
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: lhs_val,
                        then_block: rhs_block,
                        then_args: vec![],
                        else_block: join_block,
                        else_args: vec![false_val],
                    },
                );

                // In rhs_block, evaluate rhs and go to join
                self.current_block = rhs_block;
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: join_block,
                        args: vec![rhs_val],
                    },
                );

                // Continue in join block
                self.current_block = join_block;
                self.cache(air_ref, result_param);
                ExprResult {
                    value: Some(result_param),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Or(lhs, rhs) => {
                // Short-circuit: if lhs is true, result is true
                let lhs_val = self.lower_inst(*lhs).value.unwrap();

                let rhs_block = self.cfg.new_block();
                let join_block = self.cfg.new_block();

                // Add block parameter for the result
                let result_param = self.cfg.add_block_param(join_block, Type::Bool);

                // Branch: if lhs is true, go to join with true; else evaluate rhs
                let true_val = self.emit(CfgInstData::BoolConst(true), Type::Bool, span);
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: lhs_val,
                        then_block: join_block,
                        then_args: vec![true_val],
                        else_block: rhs_block,
                        else_args: vec![],
                    },
                );

                // In rhs_block, evaluate rhs and go to join
                self.current_block = rhs_block;
                let rhs_val = self.lower_inst(*rhs).value.unwrap();
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: join_block,
                        args: vec![rhs_val],
                    },
                );

                // Continue in join block
                self.current_block = join_block;
                self.cache(air_ref, result_param);
                ExprResult {
                    value: Some(result_param),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Neg(operand) => {
                let op_val = self.lower_inst(*operand).value.unwrap();
                let value = self.emit(CfgInstData::Neg(op_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Not(operand) => {
                let op_val = self.lower_inst(*operand).value.unwrap();
                let value = self.emit(CfgInstData::Not(op_val), ty, span);
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Alloc { slot, init } => {
                let init_result = self.lower_inst(*init);
                // If init produces a value, use it; otherwise use a dummy Unit value
                let init_val = init_result
                    .value
                    .unwrap_or_else(|| self.emit(CfgInstData::Const(0), Type::Unit, span));
                self.emit(
                    CfgInstData::Alloc {
                        slot: *slot,
                        init: init_val,
                    },
                    Type::Unit,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Load { slot } => {
                let value = self.emit(CfgInstData::Load { slot: *slot }, ty, span);
                // Don't cache loads - they need to be re-evaluated
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Store { slot, value } => {
                let val = self.lower_inst(*value).value.unwrap();
                self.emit(
                    CfgInstData::Store {
                        slot: *slot,
                        value: val,
                    },
                    Type::Unit,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Call { name, args } => {
                let mut arg_vals = Vec::new();
                for arg in args {
                    arg_vals.push(self.lower_inst(*arg).value.unwrap());
                }
                let value = self.emit(
                    CfgInstData::Call {
                        name: name.clone(),
                        args: arg_vals,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Intrinsic { name, args } => {
                let mut arg_vals = Vec::new();
                for arg in args {
                    arg_vals.push(self.lower_inst(*arg).value.unwrap());
                }
                let value = self.emit(
                    CfgInstData::Intrinsic {
                        name: name.clone(),
                        args: arg_vals,
                    },
                    ty,
                    span,
                );
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::StructInit { struct_id, fields } => {
                let mut field_vals = Vec::new();
                for field in fields {
                    field_vals.push(self.lower_inst(*field).value.unwrap());
                }
                let value = self.emit(
                    CfgInstData::StructInit {
                        struct_id: *struct_id,
                        fields: field_vals,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                let base_val = self.lower_inst(*base).value.unwrap();
                let value = self.emit(
                    CfgInstData::FieldGet {
                        base: base_val,
                        struct_id: *struct_id,
                        field_index: *field_index,
                    },
                    ty,
                    span,
                );
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::FieldSet {
                slot,
                struct_id,
                field_index,
                value,
            } => {
                let val = self.lower_inst(*value).value.unwrap();
                self.emit(
                    CfgInstData::FieldSet {
                        slot: *slot,
                        struct_id: *struct_id,
                        field_index: *field_index,
                        value: val,
                    },
                    Type::Unit,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Block { statements, value } => {
                // Lower each statement.
                //
                // Design decision: When a statement diverges (break/continue/return), we only
                // warn about the *first* unreachable statement or value expression following it.
                // This matches Rust's behavior and avoids flooding the user with redundant
                // warnings for code like:
                //   break;
                //   x = 1;  // warn about this
                //   y = 2;  // don't warn about this (already covered by first warning)
                for (i, stmt) in statements.iter().enumerate() {
                    let result = self.lower_inst(*stmt);
                    if matches!(result.continuation, Continuation::Diverged) {
                        // Get the span of the diverging statement for the secondary label
                        let diverging_span = self.air.get(*stmt).span;

                        // Check if there are remaining statements or a value expression
                        // that will never be executed
                        let remaining = &statements[i + 1..];
                        if !remaining.is_empty() {
                            // Warn about the first unreachable statement
                            let unreachable_stmt = remaining[0];
                            let unreachable_span = self.air.get(unreachable_stmt).span;
                            self.warnings.push(
                                CompileWarning::new(WarningKind::UnreachableCode, unreachable_span)
                                    .with_label(
                                        "any code following this expression is unreachable",
                                        diverging_span,
                                    )
                                    .with_note(
                                        "this warning occurs because the preceding expression \
                                         diverges (e.g., returns, breaks, or continues)",
                                    ),
                            );
                        } else {
                            // The final value expression is unreachable.
                            // However, don't warn about synthetic unit values (created by parser
                            // when a block has no trailing expression). These have zero-length
                            // spans pointing at the closing brace.
                            let value_span = self.air.get(*value).span;
                            let is_synthetic = value_span.start == value_span.end;
                            if !is_synthetic {
                                self.warnings.push(
                                    CompileWarning::new(WarningKind::UnreachableCode, value_span)
                                        .with_label(
                                            "any code following this expression is unreachable",
                                            diverging_span,
                                        )
                                        .with_note(
                                            "this warning occurs because the preceding expression \
                                             diverges (e.g., returns, breaks, or continues)",
                                        ),
                                );
                            }
                        }
                        return ExprResult {
                            value: None,
                            continuation: Continuation::Diverged,
                        };
                    }
                }

                // Lower the final value
                self.lower_inst(*value)
            }

            AirInstData::Branch {
                cond,
                then_value,
                else_value,
            } => {
                let cond_val = self.lower_inst(*cond).value.unwrap();

                let then_block = self.cfg.new_block();
                let else_block = self.cfg.new_block();
                let join_block = self.cfg.new_block();

                // Get types for then/else
                let then_type = self.air.get(*then_value).ty;
                let else_type = else_value.map(|e| self.air.get(e).ty);

                // Branch to then/else
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: cond_val,
                        then_block,
                        then_args: vec![],
                        else_block,
                        else_args: vec![],
                    },
                );

                // Lower then branch
                self.current_block = then_block;
                let then_result = self.lower_inst(*then_value);
                let then_exit_block = self.current_block;
                let then_diverged = matches!(then_result.continuation, Continuation::Diverged);

                // Lower else branch
                self.current_block = else_block;
                let else_result = if let Some(else_val) = else_value {
                    self.lower_inst(*else_val)
                } else {
                    // No else - emit unit
                    let unit_val = self.emit(CfgInstData::Const(0), Type::Unit, span);
                    ExprResult {
                        value: Some(unit_val),
                        continuation: Continuation::Continues,
                    }
                };
                let else_exit_block = self.current_block;
                let else_diverged = matches!(else_result.continuation, Continuation::Diverged);

                // If both branches diverge, mark join block as unreachable and diverge
                if then_diverged && else_diverged {
                    self.cfg.set_terminator(join_block, Terminator::Unreachable);
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }

                // Determine result type
                let result_type = if then_type.is_never() {
                    else_type.unwrap_or(Type::Unit)
                } else {
                    then_type
                };

                // Add block parameter for result (if we have a value type)
                let result_param = if result_type != Type::Unit && result_type != Type::Never {
                    Some(self.cfg.add_block_param(join_block, result_type))
                } else {
                    None
                };

                // Wire up non-divergent branches to join
                if !then_diverged {
                    let args = if let Some(val) = then_result.value {
                        if result_param.is_some() {
                            vec![val]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    self.cfg.set_terminator(
                        then_exit_block,
                        Terminator::Goto {
                            target: join_block,
                            args,
                        },
                    );
                }

                if !else_diverged {
                    let args = if let Some(val) = else_result.value {
                        if result_param.is_some() {
                            vec![val]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    self.cfg.set_terminator(
                        else_exit_block,
                        Terminator::Goto {
                            target: join_block,
                            args,
                        },
                    );
                }

                self.current_block = join_block;

                if let Some(param) = result_param {
                    self.cache(air_ref, param);
                }

                ExprResult {
                    value: result_param,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Loop { cond, body } => {
                let header_block = self.cfg.new_block();
                let body_block = self.cfg.new_block();
                let exit_block = self.cfg.new_block();

                // Jump to header
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: header_block,
                        args: vec![],
                    },
                );

                // Push loop context
                self.loop_stack.push(LoopContext {
                    header: header_block,
                    exit: exit_block,
                });

                // Lower condition in header
                self.current_block = header_block;
                let cond_val = self.lower_inst(*cond).value.unwrap();

                // Branch: if true go to body, if false exit
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Branch {
                        cond: cond_val,
                        then_block: body_block,
                        then_args: vec![],
                        else_block: exit_block,
                        else_args: vec![],
                    },
                );

                // Lower body
                self.current_block = body_block;
                let body_result = self.lower_inst(*body);

                // After body, go back to header (unless diverged)
                if !matches!(body_result.continuation, Continuation::Diverged) {
                    self.cfg.set_terminator(
                        self.current_block,
                        Terminator::Goto {
                            target: header_block,
                            args: vec![],
                        },
                    );
                }

                self.loop_stack.pop();

                // Continue after loop
                self.current_block = exit_block;

                // Loops produce a unit value (for use in unit-returning functions)
                let unit_val = self.emit(CfgInstData::Const(0), Type::Unit, span);
                ExprResult {
                    value: Some(unit_val),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::InfiniteLoop { body } => {
                // Infinite loop: loop { body }
                //
                // Structure (2 blocks, not 3):
                //   body_block: execute body, then goto body_block
                //   exit_block: only reachable via break
                //
                // Unlike while loops, there's no condition check, so we don't need
                // a separate header block. The body_block serves as both the loop
                // entry point and the continue target.
                let body_block = self.cfg.new_block();
                let exit_block = self.cfg.new_block();

                // Jump to body
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: body_block,
                        args: vec![],
                    },
                );

                // Push loop context (body_block is the continue target)
                self.loop_stack.push(LoopContext {
                    header: body_block,
                    exit: exit_block,
                });

                // Lower body
                self.current_block = body_block;
                let body_result = self.lower_inst(*body);

                // After body, go back to start (unless diverged via return/break/continue)
                if !matches!(body_result.continuation, Continuation::Diverged) {
                    self.cfg.set_terminator(
                        self.current_block,
                        Terminator::Goto {
                            target: body_block,
                            args: vec![],
                        },
                    );
                }

                self.loop_stack.pop();

                // Continue after loop (only reachable via break).
                // Set Unreachable as the initial terminator. If there's code after the loop
                // (which requires a break to be reachable), the subsequent Ret instruction
                // will overwrite this with the correct Return terminator. If there's no break,
                // the block is truly unreachable and Unreachable is correct.
                self.current_block = exit_block;
                self.cfg
                    .set_terminator(self.current_block, Terminator::Unreachable);

                // Infinite loops have Never type, but if we reach exit_block via break,
                // we need a dummy unit value for the loop expression result.
                let unit_val = self.emit(CfgInstData::Const(0), Type::Unit, span);
                ExprResult {
                    value: Some(unit_val),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Match { scrutinee, arms } => {
                // Lower the scrutinee
                let scrutinee_val = self.lower_inst(*scrutinee).value.unwrap();

                // Create blocks for each arm and a join block
                let arm_blocks: Vec<_> = arms.iter().map(|_| self.cfg.new_block()).collect();
                let join_block = self.cfg.new_block();

                // Get result type (from first non-Never arm)
                let result_type = arms
                    .iter()
                    .map(|(_, body)| self.air.get(*body).ty)
                    .find(|ty| !ty.is_never())
                    .unwrap_or(Type::Never);

                // Create the switch terminator
                // Build cases: for each arm, check pattern and jump to corresponding block
                let mut switch_cases = Vec::new();
                let mut default_block = None;

                for (i, (pattern, _)) in arms.iter().enumerate() {
                    match pattern {
                        AirPattern::Wildcard => {
                            default_block = Some(arm_blocks[i]);
                            // Wildcard matches everything - any patterns after this are unreachable
                            break;
                        }
                        AirPattern::Int(n) => {
                            switch_cases.push((*n, arm_blocks[i]));
                        }
                        AirPattern::Bool(b) => {
                            // Booleans are 0 or 1
                            let val = if *b { 1 } else { 0 };
                            switch_cases.push((val, arm_blocks[i]));
                        }
                    }
                }

                // If no explicit wildcard, use the last arm as default
                // This handles exhaustive matches like `true => ..., false => ...`
                // where semantics verified exhaustiveness but we need a default for codegen
                let default = default_block.unwrap_or_else(|| {
                    // Pop the last case to use as default
                    let (_, last_block) = switch_cases
                        .pop()
                        .expect("match must have at least one arm");
                    last_block
                });

                // Set the switch terminator on current block
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Switch {
                        scrutinee: scrutinee_val,
                        cases: switch_cases,
                        default,
                    },
                );

                // Lower each arm and wire to join block
                let mut all_diverged = true;
                let mut arm_results = Vec::new();

                for (i, (_, body)) in arms.iter().enumerate() {
                    self.current_block = arm_blocks[i];
                    let body_result = self.lower_inst(*body);
                    let exit_block = self.current_block;
                    let diverged = matches!(body_result.continuation, Continuation::Diverged);

                    if !diverged {
                        all_diverged = false;
                    }

                    arm_results.push((exit_block, body_result, diverged));
                }

                // If all arms diverge, mark join block unreachable
                if all_diverged {
                    self.cfg.set_terminator(join_block, Terminator::Unreachable);
                    return ExprResult {
                        value: None,
                        continuation: Continuation::Diverged,
                    };
                }

                // Add block parameter for result (if we have a value type)
                let result_param = if result_type != Type::Unit && result_type != Type::Never {
                    Some(self.cfg.add_block_param(join_block, result_type))
                } else {
                    None
                };

                // Wire up non-divergent arms to join
                for (exit_block, body_result, diverged) in arm_results {
                    if !diverged {
                        let args = if let Some(val) = body_result.value {
                            if result_param.is_some() {
                                vec![val]
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        };
                        self.cfg.set_terminator(
                            exit_block,
                            Terminator::Goto {
                                target: join_block,
                                args,
                            },
                        );
                    }
                }

                self.current_block = join_block;

                if let Some(param) = result_param {
                    self.cache(air_ref, param);
                }

                ExprResult {
                    value: result_param,
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::Break => {
                let ctx = self.loop_stack.last().expect("break outside loop");
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: ctx.exit,
                        args: vec![],
                    },
                );

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::Continue => {
                let ctx = self.loop_stack.last().expect("continue outside loop");
                self.cfg.set_terminator(
                    self.current_block,
                    Terminator::Goto {
                        target: ctx.header,
                        args: vec![],
                    },
                );

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::Ret(value) => {
                let val = match value {
                    Some(v) => {
                        let result = self.lower_inst(*v);
                        if matches!(result.continuation, Continuation::Diverged) {
                            // The return value expression itself diverged (e.g., a block
                            // containing an earlier return). The terminator was already set
                            // by the inner diverging expression, so just propagate divergence.
                            return ExprResult {
                                value: None,
                                continuation: Continuation::Diverged,
                            };
                        }
                        Some(result.value.unwrap())
                    }
                    None => None,
                };
                self.cfg
                    .set_terminator(self.current_block, Terminator::Return { value: val });

                ExprResult {
                    value: None,
                    continuation: Continuation::Diverged,
                }
            }

            AirInstData::ArrayInit {
                array_type_id,
                elements,
            } => {
                let mut element_vals = Vec::new();
                for elem in elements {
                    element_vals.push(self.lower_inst(*elem).value.unwrap());
                }
                let value = self.emit(
                    CfgInstData::ArrayInit {
                        array_type_id: *array_type_id,
                        elements: element_vals,
                    },
                    ty,
                    span,
                );
                self.cache(air_ref, value);
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::IndexGet {
                base,
                array_type_id,
                index,
            } => {
                let base_val = self.lower_inst(*base).value.unwrap();
                let index_val = self.lower_inst(*index).value.unwrap();
                let value = self.emit(
                    CfgInstData::IndexGet {
                        base: base_val,
                        array_type_id: *array_type_id,
                        index: index_val,
                    },
                    ty,
                    span,
                );
                ExprResult {
                    value: Some(value),
                    continuation: Continuation::Continues,
                }
            }

            AirInstData::IndexSet {
                slot,
                array_type_id,
                index,
                value,
            } => {
                let index_val = self.lower_inst(*index).value.unwrap();
                let val = self.lower_inst(*value).value.unwrap();
                self.emit(
                    CfgInstData::IndexSet {
                        slot: *slot,
                        array_type_id: *array_type_id,
                        index: index_val,
                        value: val,
                    },
                    Type::Unit,
                    span,
                );
                ExprResult {
                    value: None,
                    continuation: Continuation::Continues,
                }
            }
        }
    }

    /// Emit an instruction in the current block.
    fn emit(&mut self, data: CfgInstData, ty: Type, span: rue_span::Span) -> CfgValue {
        self.cfg
            .add_inst_to_block(self.current_block, CfgInst { data, ty, span })
    }

    /// Cache a value for an AIR ref.
    fn cache(&mut self, air_ref: AirRef, value: CfgValue) {
        self.value_cache[air_ref.as_u32() as usize] = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_air::Sema;
    use rue_intern::Interner;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn build_cfg(source: &str) -> Cfg {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        let mut interner = Interner::new();
        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let mut sema = Sema::new(&rir, &mut interner);
        let output = sema.analyze_all().unwrap();

        let func = &output.functions[0];
        CfgBuilder::build(&func.air, func.num_locals, func.num_param_slots, &func.name).cfg
    }

    #[test]
    fn test_simple_return() {
        let cfg = build_cfg("fn main() -> i32 { 42 }");

        assert_eq!(cfg.block_count(), 1);
        assert_eq!(cfg.fn_name(), "main");

        let entry = cfg.get_block(cfg.entry);
        assert!(matches!(entry.terminator, Terminator::Return { .. }));
    }

    #[test]
    fn test_if_else() {
        let cfg = build_cfg("fn main() -> i32 { if true { 1 } else { 2 } }");

        // Should have: entry, then, else, join
        assert!(cfg.block_count() >= 3);
    }

    #[test]
    fn test_while_loop() {
        let cfg = build_cfg("fn main() -> i32 { let mut x = 0; while x < 10 { x = x + 1; } x }");

        // Should have: entry, header, body, exit, and possibly join blocks
        assert!(cfg.block_count() >= 3);
    }

    #[test]
    fn test_short_circuit_and() {
        let cfg = build_cfg("fn main() -> i32 { if true && false { 1 } else { 0 } }");

        // && creates extra blocks for short-circuit evaluation
        assert!(cfg.block_count() >= 3);
    }
}
