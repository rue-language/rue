//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.

use std::collections::{HashMap, HashSet};

use crate::inst::{Air, AirInst, AirInstData, AirRef};
use crate::types::{StructDef, StructField, StructId, Type};
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, WarningKind};
use rue_intern::{Interner, Symbol};
use rue_rir::{InstData, InstRef, Rir};
use rue_span::Span;

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
    /// Number of local variable slots needed
    pub num_locals: u32,
    /// Number of ABI slots used by parameters.
    /// For scalar types (i32, bool), each parameter uses 1 slot.
    /// For struct types, each field uses 1 slot (flattened ABI).
    pub num_param_slots: u32,
}

/// Output from semantic analysis.
///
/// Contains all analyzed functions, struct definitions, and any warnings
/// generated during analysis.
#[derive(Debug)]
pub struct SemaOutput {
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// Struct definitions.
    pub struct_defs: Vec<StructDef>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

/// Information about a local variable.
#[derive(Debug, Clone)]
struct LocalVar {
    /// Slot index for this variable
    slot: u32,
    /// Type of the variable
    ty: Type,
    /// Whether the variable is mutable
    is_mut: bool,
    /// Span of the variable declaration (for unused variable warnings)
    span: Span,
}

/// Information about a function parameter.
#[derive(Debug, Clone)]
struct ParamInfo {
    /// Starting ABI slot for this parameter (0-based).
    /// For scalar types, this is the single slot.
    /// For struct types, this is the first field's slot.
    abi_slot: u32,
    /// Parameter type
    ty: Type,
}

/// Context for analyzing instructions within a function.
///
/// Bundles together the mutable state that needs to be threaded through
/// recursive `analyze_inst` calls.
struct AnalysisContext<'a> {
    /// Local variables in scope
    locals: HashMap<Symbol, LocalVar>,
    /// Function parameters (immutable reference, shared across the function)
    params: &'a HashMap<Symbol, ParamInfo>,
    /// Next available slot for local variables
    next_slot: u32,
    /// How many loops we're nested inside (for break/continue validation)
    loop_depth: u32,
    /// Local variables that have been read (for unused variable detection)
    used_locals: HashSet<Symbol>,
    /// Return type of the current function (for explicit return validation)
    return_type: Type,
}

/// Information about a function.
#[derive(Debug, Clone)]
struct FunctionInfo {
    /// Parameter types (in order)
    param_types: Vec<Type>,
    /// Return type
    return_type: Type,
}

/// Describes what type we expect from an expression during type checking.
///
/// This enables bidirectional type checking in a single pass:
/// - `Check(ty)`: We know the expected type (top-down), verify the expression matches
/// - `Synthesize`: We don't know the type, infer it from the expression (bottom-up)
#[derive(Debug, Clone, Copy)]
enum TypeExpectation {
    /// We have a specific type we're checking against (top-down).
    /// The expression MUST have this type or be coercible to it.
    Check(Type),

    /// We don't know the type yet - synthesize it (bottom-up).
    /// The expression determines its own type.
    Synthesize,
}

impl TypeExpectation {
    /// Get the type to use for integer literals.
    /// Returns the expected type if it's an integer, otherwise defaults to i32.
    fn integer_type(&self) -> Type {
        match self {
            TypeExpectation::Check(ty) if ty.is_integer() => *ty,
            _ => Type::I32,
        }
    }

    /// Check if a synthesized type is compatible with this expectation.
    /// Returns Ok(()) if compatible, or a type mismatch error if not.
    fn check(&self, synthesized: Type, span: Span) -> CompileResult<()> {
        match self {
            TypeExpectation::Synthesize => Ok(()),
            TypeExpectation::Check(expected) => {
                if synthesized == *expected
                    || *expected == Type::Unit // Unit context accepts anything
                    || synthesized.is_never() // Never coerces to anything
                    || expected.is_error()
                    || synthesized.is_error()
                {
                    Ok(())
                } else {
                    Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: expected.name().to_string(),
                            found: synthesized.name().to_string(),
                        },
                        span,
                    ))
                }
            }
        }
    }

    /// Returns true if this is a Check expectation for the Unit type.
    fn is_unit_context(&self) -> bool {
        matches!(self, TypeExpectation::Check(Type::Unit))
    }
}

/// Result of analyzing an instruction: the AIR reference and its synthesized type.
#[derive(Debug, Clone, Copy)]
struct AnalysisResult {
    /// Reference to the generated AIR instruction
    air_ref: AirRef,
    /// The synthesized type of this expression
    ty: Type,
}

impl AnalysisResult {
    #[must_use]
    fn new(air_ref: AirRef, ty: Type) -> Self {
        Self { air_ref, ty }
    }
}

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    rir: &'a Rir,
    interner: &'a Interner,
    /// Function table: maps function name symbols to their info
    functions: HashMap<Symbol, FunctionInfo>,
    /// Struct table: maps struct name symbols to their StructId
    structs: HashMap<Symbol, StructId>,
    /// Struct definitions indexed by StructId
    struct_defs: Vec<StructDef>,
    /// Warnings collected during analysis
    warnings: Vec<CompileWarning>,
}

impl<'a> Sema<'a> {
    /// Create a new semantic analyzer.
    pub fn new(rir: &'a Rir, interner: &'a Interner) -> Self {
        Self {
            rir,
            interner,
            functions: HashMap::new(),
            structs: HashMap::new(),
            struct_defs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Check for unused local variables in the current scope.
    /// `saved_locals` contains the locals from the outer scope before this scope started.
    /// We check variables that are in `ctx.locals` but not in `saved_locals` (i.e., new in this scope).
    fn check_unused_locals_in_scope(
        &mut self,
        saved_locals: &HashMap<Symbol, LocalVar>,
        ctx: &AnalysisContext,
    ) {
        for (symbol, local) in &ctx.locals {
            // Skip if this variable existed in the outer scope
            if saved_locals.contains_key(symbol) {
                continue;
            }

            // Skip if variable was used
            if ctx.used_locals.contains(symbol) {
                continue;
            }

            // Get variable name
            let name = self.interner.get(*symbol);

            // Skip variables starting with underscore (convention for intentionally unused)
            if name.starts_with('_') {
                continue;
            }

            // Emit warning
            self.warnings.push(CompileWarning::new(
                WarningKind::UnusedVariable(name.to_string()),
                local.span,
            ));
        }
    }

    /// Analyze all functions in the RIR.
    ///
    /// Consumes the Sema and returns a [`SemaOutput`] containing all analyzed
    /// functions, struct definitions, and any warnings generated during analysis.
    pub fn analyze_all(mut self) -> CompileResult<SemaOutput> {
        // First pass: collect struct definitions (needed for type resolution)
        self.collect_struct_definitions()?;

        // Second pass: collect function signatures
        self.collect_function_signatures()?;

        // Third pass: analyze function bodies
        let mut functions = Vec::new();

        for (_, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                name,
                params,
                return_type,
                body,
            } = &inst.data
            {
                let fn_name = self.interner.get(*name).to_string();
                let ret_type = self.resolve_type(*return_type, inst.span)?;

                // Resolve parameter types
                let param_info: Vec<(Symbol, Type)> = params
                    .iter()
                    .map(|(pname, ptype)| {
                        let ty = self.resolve_type(*ptype, inst.span)?;
                        Ok((*pname, ty))
                    })
                    .collect::<CompileResult<Vec<_>>>()?;

                let (air, num_locals, num_param_slots) =
                    self.analyze_function(ret_type, &param_info, *body)?;

                functions.push(AnalyzedFunction {
                    name: fn_name,
                    air,
                    num_locals,
                    num_param_slots,
                });
            }
        }

        Ok(SemaOutput {
            functions,
            struct_defs: self.struct_defs,
            warnings: self.warnings,
        })
    }

    /// Collect all struct definitions from the RIR.
    fn collect_struct_definitions(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl { name, fields } = &inst.data {
                let struct_id = StructId(self.struct_defs.len() as u32);
                let struct_name = self.interner.get(*name).to_string();

                // Resolve field types (can only be primitive types for now, or other structs)
                let mut resolved_fields = Vec::new();
                for (field_name, field_type) in fields {
                    let field_ty = self.resolve_type(*field_type, inst.span)?;
                    resolved_fields.push(StructField {
                        name: self.interner.get(*field_name).to_string(),
                        ty: field_ty,
                    });
                }

                self.struct_defs.push(StructDef {
                    name: struct_name,
                    fields: resolved_fields,
                });
                self.structs.insert(*name, struct_id);
            }
        }
        Ok(())
    }

    /// Collect all function signatures for forward reference
    fn collect_function_signatures(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                name,
                params,
                return_type,
                ..
            } = &inst.data
            {
                let ret_type = self.resolve_type(*return_type, inst.span)?;
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|(_, ptype)| self.resolve_type(*ptype, inst.span))
                    .collect::<CompileResult<Vec<_>>>()?;

                self.functions.insert(
                    *name,
                    FunctionInfo {
                        param_types,
                        return_type: ret_type,
                    },
                );
            }
        }
        Ok(())
    }

    /// Analyze a single function, producing AIR.
    /// Returns (air, num_locals, num_param_slots).
    fn analyze_function(
        &mut self,
        return_type: Type,
        params: &[(Symbol, Type)],
        body: InstRef,
    ) -> CompileResult<(Air, u32, u32)> {
        let mut air = Air::new(return_type);
        let mut param_map: HashMap<Symbol, ParamInfo> = HashMap::new();

        // Add parameters to the param map, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype) in params.iter() {
            param_map.insert(
                *pname,
                ParamInfo {
                    abi_slot: next_abi_slot,
                    ty: *ptype,
                },
            );
            next_abi_slot += self.abi_slot_count(*ptype);
        }
        let num_param_slots = next_abi_slot;

        // Create analysis context
        let mut ctx = AnalysisContext {
            locals: HashMap::new(),
            params: &param_map,
            next_slot: 0,
            loop_depth: 0,
            used_locals: HashSet::new(),
            return_type,
        };

        // Analyze the body expression
        let body_result = self.analyze_inst(
            &mut air,
            body,
            TypeExpectation::Check(return_type),
            &mut ctx,
        )?;

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::Never {
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(body_result.air_ref)),
                ty: return_type,
                span: self.rir.get(body).span,
            });
        }

        Ok((air, ctx.next_slot, num_param_slots))
    }

    /// Analyze an RIR instruction, producing AIR instructions.
    ///
    /// Uses bidirectional type checking: when `expectation` is `Check(ty)`, validates
    /// that the result is compatible with `ty`. When `Synthesize`, infers the type.
    /// Returns both the AIR reference and the synthesized type.
    fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        expectation: TypeExpectation,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            InstData::IntConst(value) => {
                // Integer constants adopt the expected type if it's an integer, else default to i32
                let ty = expectation.integer_type();
                expectation.check(ty, inst.span)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Const(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::BoolConst(value) => {
                let ty = Type::Bool;
                expectation.check(ty, inst.span)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::BoolConst(*value),
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Add { lhs, rhs } => self.analyze_binary_arith(
                air,
                *lhs,
                *rhs,
                expectation,
                AirInstData::Add,
                inst.span,
                ctx,
            ),

            InstData::Sub { lhs, rhs } => self.analyze_binary_arith(
                air,
                *lhs,
                *rhs,
                expectation,
                AirInstData::Sub,
                inst.span,
                ctx,
            ),

            InstData::Mul { lhs, rhs } => self.analyze_binary_arith(
                air,
                *lhs,
                *rhs,
                expectation,
                AirInstData::Mul,
                inst.span,
                ctx,
            ),

            InstData::Div { lhs, rhs } => self.analyze_binary_arith(
                air,
                *lhs,
                *rhs,
                expectation,
                AirInstData::Div,
                inst.span,
                ctx,
            ),

            InstData::Mod { lhs, rhs } => self.analyze_binary_arith(
                air,
                *lhs,
                *rhs,
                expectation,
                AirInstData::Mod,
                inst.span,
                ctx,
            ),

            // Comparison operators: operands must be the same type, result is bool.
            // We synthesize the type from the left operand and check the right against it.
            // Never and Error types are propagated without additional errors.
            // Equality operators (==, !=) also allow bool operands.
            InstData::Eq { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Eq, inst.span, ctx)
            }

            InstData::Ne { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Ne, inst.span, ctx)
            }

            InstData::Lt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Lt, inst.span, ctx)
            }

            InstData::Gt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Gt, inst.span, ctx)
            }

            InstData::Le { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Le, inst.span, ctx)
            }

            InstData::Ge { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Ge, inst.span, ctx)
            }

            // Logical operators: operands and result are all bool
            InstData::And { lhs, rhs } => {
                let lhs_result =
                    self.analyze_inst(air, *lhs, TypeExpectation::Check(Type::Bool), ctx)?;
                let rhs_result =
                    self.analyze_inst(air, *rhs, TypeExpectation::Check(Type::Bool), ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::Or { lhs, rhs } => {
                let lhs_result =
                    self.analyze_inst(air, *lhs, TypeExpectation::Check(Type::Bool), ctx)?;
                let rhs_result =
                    self.analyze_inst(air, *rhs, TypeExpectation::Check(Type::Bool), ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::Neg { operand } => {
                // Special case: -2147483648 (MIN_I32)
                // The literal 2147483648 exceeds i32::MAX, but -2147483648 is valid.
                // We check this first regardless of expectation mode.
                let operand_inst = self.rir.get(*operand);
                if let InstData::IntConst(value) = &operand_inst.data {
                    if *value == 2147483648 {
                        // Determine what type to use
                        let ty = match expectation {
                            TypeExpectation::Check(t) if t.is_integer() => t,
                            _ => Type::I32,
                        };
                        if ty == Type::I32 {
                            let air_ref = air.add_inst(AirInst {
                                data: AirInstData::Const(-2147483648_i64),
                                ty,
                                span: inst.span,
                            });
                            return Ok(AnalysisResult::new(air_ref, ty));
                        }
                    }
                }

                // Determine the type: use expected type if integer, otherwise synthesize from operand
                let (operand_result, op_type) = match expectation {
                    TypeExpectation::Check(ty) if ty.is_integer() => {
                        let result =
                            self.analyze_inst(air, *operand, TypeExpectation::Check(ty), ctx)?;
                        (result, ty)
                    }
                    _ => {
                        // Synthesize from operand
                        let result =
                            self.analyze_inst(air, *operand, TypeExpectation::Synthesize, ctx)?;
                        let ty = if result.ty.is_integer() {
                            result.ty
                        } else {
                            Type::I32
                        };
                        (result, ty)
                    }
                };

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Neg(operand_result.air_ref),
                    ty: op_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, op_type))
            }

            InstData::Not { operand } => {
                let operand_result =
                    self.analyze_inst(air, *operand, TypeExpectation::Check(Type::Bool), ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Not(operand_result.air_ref),
                    ty: Type::Bool,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Bool))
            }

            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // Condition must be bool
                let cond_result =
                    self.analyze_inst(air, *cond, TypeExpectation::Check(Type::Bool), ctx)?;

                // Determine the result type:
                // - If else is present, both branches must have compatible types
                //   (Never type can coerce to any type)
                // - If else is absent, the result is Unit
                if let Some(else_b) = else_block {
                    // Save locals before then branch
                    let saved_locals = ctx.locals.clone();

                    // Analyze then branch with the expected type
                    let then_result = self.analyze_inst(air, *then_block, expectation, ctx)?;
                    let then_type = then_result.ty;

                    // Restore locals and analyze else branch
                    // If then branch is Never, use original expectation for else (so it determines the result)
                    // Otherwise use then_type as the expectation
                    ctx.locals = saved_locals.clone();
                    let else_expectation = if then_type.is_never() {
                        expectation
                    } else {
                        TypeExpectation::Check(then_type)
                    };
                    let else_result = self.analyze_inst(air, *else_b, else_expectation, ctx)?;
                    let else_type = else_result.ty;

                    // Compute the unified result type using never type coercion:
                    // - If both branches are Never, result is Never
                    // - If one branch is Never, result is the other branch's type
                    // - Otherwise, types must match exactly
                    let result_type = match (then_type.is_never(), else_type.is_never()) {
                        (true, true) => Type::Never,
                        (true, false) => else_type,
                        (false, true) => then_type,
                        (false, false) => {
                            // Neither diverges - types must match exactly
                            if then_type != else_type
                                && !then_type.is_error()
                                && !else_type.is_error()
                            {
                                return Err(CompileError::new(
                                    ErrorKind::TypeMismatch {
                                        expected: then_type.name().to_string(),
                                        found: else_type.name().to_string(),
                                    },
                                    self.rir.get(*else_b).span,
                                ));
                            }
                            then_type
                        }
                    };

                    // Restore locals to original (branches are isolated scopes)
                    ctx.locals = saved_locals;

                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Branch {
                            cond: cond_result.air_ref,
                            then_value: then_result.air_ref,
                            else_value: Some(else_result.air_ref),
                        },
                        ty: result_type,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, result_type))
                } else {
                    // No else branch - result is Unit
                    // Save locals
                    let saved_locals = ctx.locals.clone();

                    // Analyze then branch (can be any type, we'll ignore it)
                    let then_result = self.analyze_inst(
                        air,
                        *then_block,
                        TypeExpectation::Check(Type::Unit),
                        ctx,
                    )?;

                    // Restore locals
                    ctx.locals = saved_locals;

                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Branch {
                            cond: cond_result.air_ref,
                            then_value: then_result.air_ref,
                            else_value: None,
                        },
                        ty: Type::Unit,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, Type::Unit))
                }
            }

            InstData::Loop { cond, body } => {
                // While loop: condition must be bool, result is Unit
                expectation.check(Type::Unit, inst.span)?;

                let cond_result =
                    self.analyze_inst(air, *cond, TypeExpectation::Check(Type::Bool), ctx)?;

                // Save locals before loop body
                let saved_locals = ctx.locals.clone();

                // Analyze body - while body is Unit type
                // Increment loop_depth so break/continue inside the body are valid
                ctx.loop_depth += 1;
                let body_result =
                    self.analyze_inst(air, *body, TypeExpectation::Check(Type::Unit), ctx)?;
                ctx.loop_depth -= 1;

                // Restore locals (loop body is its own scope)
                ctx.locals = saved_locals;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Loop {
                        cond: cond_result.air_ref,
                        body: body_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::InfiniteLoop { body } => {
                // Infinite loop: `loop { body }` - always produces Never type
                // The loop never terminates normally (only via break, which is handled separately)

                // Save locals before loop body
                let saved_locals = ctx.locals.clone();

                // Analyze body - loop body is Unit type
                // Increment loop_depth so break/continue inside the body are valid
                ctx.loop_depth += 1;
                let body_result =
                    self.analyze_inst(air, *body, TypeExpectation::Check(Type::Unit), ctx)?;
                ctx.loop_depth -= 1;

                // Restore locals (loop body is its own scope)
                ctx.locals = saved_locals;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::InfiniteLoop {
                        body: body_result.air_ref,
                    },
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Alloc {
                name,
                is_mut,
                ty,
                init,
            } => {
                // Determine the type from annotation or synthesize from initializer
                let (init_result, var_type) = if let Some(type_sym) = ty {
                    // Type annotation provided: check initializer against it
                    let var_type = self.resolve_type(*type_sym, inst.span)?;
                    let init_result =
                        self.analyze_inst(air, *init, TypeExpectation::Check(var_type), ctx)?;
                    (init_result, var_type)
                } else {
                    // No annotation: synthesize type from initializer (SINGLE TRAVERSAL)
                    let init_result =
                        self.analyze_inst(air, *init, TypeExpectation::Synthesize, ctx)?;
                    (init_result, init_result.ty)
                };

                // Allocate slots - structs need multiple slots (one per field)
                let slot = ctx.next_slot;
                let num_slots = match var_type {
                    Type::Struct(struct_id) => {
                        self.struct_defs[struct_id.0 as usize].field_count() as u32
                    }
                    _ => 1,
                };
                ctx.next_slot += num_slots;

                // Register the variable (shadowing is allowed by just overwriting)
                ctx.locals.insert(
                    *name,
                    LocalVar {
                        slot,
                        ty: var_type,
                        is_mut: *is_mut,
                        span: inst.span,
                    },
                );

                // Emit the alloc instruction
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Alloc {
                        slot,
                        init: init_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::VarRef { name } => {
                // First check if it's a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    let ty = param_info.ty;
                    expectation.check(ty, inst.span)?;

                    // Emit Param with the ABI slot (not the parameter index).
                    // For struct parameters, this is the starting slot of the first field.
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Param {
                            index: param_info.abi_slot,
                        },
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }

                // Look up the variable in locals
                let name_str = self.interner.get(*name);
                let local = ctx.locals.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        inst.span,
                    )
                })?;

                let ty = local.ty;
                let slot = local.slot;

                // Mark variable as used
                ctx.used_locals.insert(*name);

                // Type check
                expectation.check(ty, inst.span)?;

                // Load the variable
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Load { slot },
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::Assign { name, value } => {
                // Look up the variable
                let name_str = self.interner.get(*name);
                let local = ctx.locals.get(name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        inst.span,
                    )
                })?;

                // Check mutability
                if !local.is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        inst.span,
                    ));
                }

                let slot = local.slot;
                let ty = local.ty;

                // Analyze the value
                let value_result =
                    self.analyze_inst(air, *value, TypeExpectation::Check(ty), ctx)?;

                // Emit store instruction
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Store {
                        slot,
                        value: value_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::Break => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::BreakOutsideLoop, inst.span));
                }

                // Break has the never type - it diverges (doesn't produce a value)
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Break,
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Continue => {
                // Validate that we're inside a loop
                if ctx.loop_depth == 0 {
                    return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
                }

                // Continue has the never type - it diverges (doesn't produce a value)
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Continue,
                    ty: Type::Never,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::FnDecl { .. } => {
                // Function declarations are handled at the top level
                unreachable!("FnDecl should not appear in expression context")
            }

            InstData::Ret(inner) => {
                // Handle `return;` without expression (only valid for unit-returning functions)
                let inner_air_ref = if let Some(inner) = inner {
                    // Explicit return with value: analyze with the function's return type
                    let inner_result = self.analyze_inst(
                        air,
                        *inner,
                        TypeExpectation::Check(ctx.return_type),
                        ctx,
                    )?;
                    let inner_ty = inner_result.ty;

                    // Type check: returned value must match function's return type.
                    // We check for error types first to avoid cascading errors - if either
                    // type is already an error, we skip the mismatch check since there's
                    // already an error reported. Note: can_coerce_to handles inner_ty being
                    // Error (returns true), but we also need to handle return_type being Error.
                    if !ctx.return_type.is_error()
                        && !inner_ty.is_error()
                        && !inner_ty.can_coerce_to(&ctx.return_type)
                    {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: ctx.return_type.name().to_string(),
                                found: inner_ty.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                    Some(inner_result.air_ref)
                } else {
                    // `return;` without expression - only valid for unit-returning functions
                    if ctx.return_type != Type::Unit && !ctx.return_type.is_error() {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: ctx.return_type.name().to_string(),
                                found: "()".to_string(),
                            },
                            inst.span,
                        ));
                    }
                    None
                };

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Ret(inner_air_ref),
                    ty: Type::Never, // Return expressions have Never type (they diverge)
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Never))
            }

            InstData::Block { extra_start, len } => {
                // Get the instruction refs from extra data
                let inst_refs = self.rir.get_extra(*extra_start, *len);

                // Save the current locals for block scoping.
                // Variables declared in this block will be removed when the block ends.
                let saved_locals = ctx.locals.clone();

                // Process all instructions in the block
                // The last one is the final expression (the block's value)
                // All other instructions are statements and should be typed as Unit
                let mut statements = Vec::new();
                let mut last_result: Option<AnalysisResult> = None;
                let num_insts = inst_refs.len();
                for (i, &raw_ref) in inst_refs.iter().enumerate() {
                    let inst_ref = InstRef::from_raw(raw_ref);
                    let is_last = i == num_insts - 1;
                    // Only the final expression should match the expectation;
                    // statements (let, assign, expr;) don't need type checking
                    // against the block's expected type.
                    // When in Unit context (e.g., while loop body), we synthesize
                    // the type for the final expression since we discard its value.
                    let inst_expectation = if is_last {
                        if expectation.is_unit_context() {
                            // In Unit context, synthesize type (don't enforce Unit on final expr)
                            TypeExpectation::Synthesize
                        } else {
                            expectation
                        }
                    } else {
                        TypeExpectation::Check(Type::Unit)
                    };
                    let result = self.analyze_inst(air, inst_ref, inst_expectation, ctx)?;

                    if is_last {
                        last_result = Some(result);
                    } else {
                        statements.push(result.air_ref);
                    }
                }

                // Check for unused variables before restoring scope
                self.check_unused_locals_in_scope(&saved_locals, ctx);

                // Restore locals to remove block-scoped variables.
                // Note: We don't restore next_slot, so slots are not reused.
                // This is a future optimization opportunity.
                ctx.locals = saved_locals;

                let last = last_result.expect("block should have at least one instruction");

                // Only create a Block instruction if there are statements;
                // otherwise just return the value directly (optimization)
                if statements.is_empty() {
                    Ok(last)
                } else {
                    // When in Unit context, the block produces Unit
                    let ty = if expectation.is_unit_context() {
                        Type::Unit
                    } else {
                        last.ty
                    };
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Block {
                            statements,
                            value: last.air_ref,
                        },
                        ty,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, ty))
                }
            }

            InstData::Call { name, args } => {
                // Look up the function
                let fn_name_str = self.interner.get(*name).to_string();
                let fn_info = self.functions.get(name).ok_or_else(|| {
                    CompileError::new(ErrorKind::UndefinedFunction(fn_name_str.clone()), inst.span)
                })?;

                // Check argument count
                if args.len() != fn_info.param_types.len() {
                    let expected = fn_info.param_types.len();
                    let found = args.len();
                    return Err(CompileError::new(
                        ErrorKind::WrongArgumentCount { expected, found },
                        inst.span,
                    ));
                }

                // Clone the data we need before mutable borrow
                let param_types = fn_info.param_types.clone();
                let return_type = fn_info.return_type;

                // Analyze arguments with expected parameter types
                let mut arg_refs = Vec::new();
                for (arg, expected_param_type) in args.iter().zip(&param_types) {
                    let arg_result = self.analyze_inst(
                        air,
                        *arg,
                        TypeExpectation::Check(*expected_param_type),
                        ctx,
                    )?;
                    arg_refs.push(arg_result.air_ref);
                }

                // Check that return type matches expectation
                expectation.check(return_type, inst.span)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Call {
                        name: fn_name_str,
                        args: arg_refs,
                    },
                    ty: return_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, return_type))
            }

            InstData::ParamRef { index: _, name } => {
                // Look up the parameter type and ABI slot from the params map
                let param_info = ctx.params.get(name).ok_or_else(|| {
                    let name_str = self.interner.get(*name);
                    CompileError::new(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        inst.span,
                    )
                })?;

                let ty = param_info.ty;
                expectation.check(ty, inst.span)?;

                // Use the ABI slot (not the RIR index) for proper struct parameter handling
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, ty))
            }

            InstData::StructDecl { .. } => {
                // Struct declarations are handled at the top level during collect_struct_definitions
                unreachable!("StructDecl should not appear in expression context")
            }

            InstData::StructInit {
                type_name,
                fields: field_inits,
            } => {
                // Look up the struct type
                let type_name_str = self.interner.get(*type_name);
                let struct_id = *self.structs.get(type_name).ok_or_else(|| {
                    CompileError::new(ErrorKind::UnknownType(type_name_str.to_string()), inst.span)
                })?;

                // Clone struct def data before mutable borrow
                let struct_def = self.struct_defs[struct_id.0 as usize].clone();
                let struct_type = Type::Struct(struct_id);

                // Type check
                expectation.check(struct_type, inst.span)?;

                // Check that all fields are provided and no extra fields
                if field_inits.len() != struct_def.fields.len() {
                    return Err(CompileError::new(
                        ErrorKind::WrongFieldCount {
                            struct_name: struct_def.name.clone(),
                            expected: struct_def.fields.len(),
                            found: field_inits.len(),
                        },
                        inst.span,
                    ));
                }

                // Analyze field values in declaration order
                // First, build a map from field name to its value
                let mut field_map: HashMap<String, InstRef> = HashMap::new();
                for (field_name, field_value) in field_inits {
                    let name_str = self.interner.get(*field_name).to_string();
                    field_map.insert(name_str, *field_value);
                }

                // Now analyze fields in struct definition order
                let mut field_refs = Vec::new();
                for struct_field in &struct_def.fields {
                    let field_value = field_map.get(&struct_field.name).ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::MissingField {
                                struct_name: struct_def.name.clone(),
                                field_name: struct_field.name.clone(),
                            },
                            inst.span,
                        )
                    })?;

                    let field_result = self.analyze_inst(
                        air,
                        *field_value,
                        TypeExpectation::Check(struct_field.ty),
                        ctx,
                    )?;
                    field_refs.push(field_result.air_ref);
                }

                // Check for extra fields
                for (field_name, _) in field_inits {
                    let name_str = self.interner.get(*field_name).to_string();
                    if struct_def.find_field(&name_str).is_none() {
                        return Err(CompileError::new(
                            ErrorKind::UnknownField {
                                struct_name: struct_def.name.clone(),
                                field_name: name_str,
                            },
                            inst.span,
                        ));
                    }
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::StructInit {
                        struct_id,
                        fields: field_refs,
                    },
                    ty: struct_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, struct_type))
            }

            InstData::FieldGet { base, field } => {
                // Synthesize the base type in a single traversal
                let base_result =
                    self.analyze_inst(air, *base, TypeExpectation::Synthesize, ctx)?;
                let base_type = base_result.ty;

                let struct_id = match base_type {
                    Type::Struct(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::FieldAccessOnNonStruct {
                                found: base_type.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                };

                let struct_def = &self.struct_defs[struct_id.0 as usize];
                let field_name_str = self.interner.get(*field).to_string();

                let (field_index, struct_field) =
                    struct_def.find_field(&field_name_str).ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::UnknownField {
                                struct_name: struct_def.name.clone(),
                                field_name: field_name_str.clone(),
                            },
                            inst.span,
                        )
                    })?;

                let field_type = struct_field.ty;

                // Type check
                expectation.check(field_type, inst.span)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::FieldGet {
                        base: base_result.air_ref,
                        struct_id,
                        field_index: field_index as u32,
                    },
                    ty: field_type,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, field_type))
            }

            InstData::FieldSet { base, field, value } => {
                // For field assignment, we need the base to be a local variable
                // Get the variable info from the base VarRef
                let base_inst = self.rir.get(*base);
                let (var_name, slot, base_type, is_mut) = match &base_inst.data {
                    InstData::VarRef { name } => {
                        let name_str = self.interner.get(*name);
                        let local = ctx.locals.get(name).ok_or_else(|| {
                            CompileError::new(
                                ErrorKind::UndefinedVariable(name_str.to_string()),
                                inst.span,
                            )
                        })?;
                        (name_str.to_string(), local.slot, local.ty, local.is_mut)
                    }
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::InvalidAssignmentTarget,
                            inst.span,
                        ));
                    }
                };

                // Check mutability
                if !is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(var_name),
                        inst.span,
                    ));
                }

                let struct_id = match base_type {
                    Type::Struct(id) => id,
                    _ => {
                        return Err(CompileError::new(
                            ErrorKind::FieldAccessOnNonStruct {
                                found: base_type.name().to_string(),
                            },
                            inst.span,
                        ));
                    }
                };

                let struct_def = &self.struct_defs[struct_id.0 as usize];
                let field_name_str = self.interner.get(*field).to_string();

                let (field_index, struct_field) =
                    struct_def.find_field(&field_name_str).ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::UnknownField {
                                struct_name: struct_def.name.clone(),
                                field_name: field_name_str.clone(),
                            },
                            inst.span,
                        )
                    })?;

                let field_type = struct_field.ty;

                // Analyze the value with the expected field type
                let value_result =
                    self.analyze_inst(air, *value, TypeExpectation::Check(field_type), ctx)?;

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::FieldSet {
                        slot,
                        struct_id,
                        field_index: field_index as u32,
                        value: value_result.air_ref,
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }

            InstData::Intrinsic { name, args } => {
                let intrinsic_name = self.interner.get(*name).to_string();

                // Currently only @dbg is supported
                if intrinsic_name != "dbg" {
                    return Err(CompileError::new(
                        ErrorKind::UnknownIntrinsic(intrinsic_name),
                        inst.span,
                    ));
                }

                // @dbg expects exactly one argument
                if args.len() != 1 {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicWrongArgCount {
                            name: intrinsic_name,
                            expected: 1,
                            found: args.len(),
                        },
                        inst.span,
                    ));
                }

                // Synthesize the argument type in a single traversal (we accept any scalar type)
                let arg_result =
                    self.analyze_inst(air, args[0], TypeExpectation::Synthesize, ctx)?;
                let arg_type = arg_result.ty;

                // Check that argument is a scalar (integer or bool)
                let is_scalar = arg_type.is_integer() || arg_type == Type::Bool;
                if !is_scalar {
                    return Err(CompileError::new(
                        ErrorKind::IntrinsicTypeMismatch {
                            name: intrinsic_name,
                            expected: "integer or bool".to_string(),
                            found: arg_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Intrinsic {
                        name: intrinsic_name,
                        args: vec![arg_result.air_ref],
                    },
                    ty: Type::Unit,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::Unit))
            }
        }
    }

    /// Resolve a type symbol to a Type.
    ///
    /// Uses symbol comparison instead of string comparison for efficiency.
    fn resolve_type(&self, type_sym: Symbol, span: Span) -> CompileResult<Type> {
        let well_known = self.interner.well_known();

        if type_sym == well_known.i8 {
            Ok(Type::I8)
        } else if type_sym == well_known.i16 {
            Ok(Type::I16)
        } else if type_sym == well_known.i32 {
            Ok(Type::I32)
        } else if type_sym == well_known.i64 {
            Ok(Type::I64)
        } else if type_sym == well_known.u8 {
            Ok(Type::U8)
        } else if type_sym == well_known.u16 {
            Ok(Type::U16)
        } else if type_sym == well_known.u32 {
            Ok(Type::U32)
        } else if type_sym == well_known.u64 {
            Ok(Type::U64)
        } else if type_sym == well_known.bool {
            Ok(Type::Bool)
        } else if type_sym == well_known.unit {
            Ok(Type::Unit)
        } else if type_sym == well_known.never {
            Ok(Type::Never)
        } else if let Some(&struct_id) = self.structs.get(&type_sym) {
            Ok(Type::Struct(struct_id))
        } else {
            let type_name = self.interner.get(type_sym);
            Err(CompileError::new(
                ErrorKind::UnknownType(type_name.to_string()),
                span,
            ))
        }
    }

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot, structs use 1 slot per field.
    fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Bool
            | Type::Unit
            | Type::Error
            | Type::Never => 1,
            Type::Struct(struct_id) => self.struct_defs[struct_id.0 as usize].field_count() as u32,
        }
    }

    /// Analyze a binary arithmetic operator (+, -, *, /, %).
    ///
    /// Follows Rust's type inference rules:
    /// - If we have a type expectation (Check mode), use that type
    /// - If synthesizing, infer the type from the left operand
    /// - Integer literals adopt the inferred type
    fn analyze_binary_arith<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        expectation: TypeExpectation,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        // Determine the operation type:
        // - If we have an expected integer type, use it
        // - Otherwise, synthesize from LHS and use that
        let (lhs_result, op_type) = match expectation {
            TypeExpectation::Check(ty) if ty.is_integer() => {
                // We know the expected type, check LHS against it
                let result = self.analyze_inst(air, lhs, TypeExpectation::Check(ty), ctx)?;
                (result, ty)
            }
            _ => {
                // Synthesize from LHS to determine the type
                let result = self.analyze_inst(air, lhs, TypeExpectation::Synthesize, ctx)?;
                let ty = if result.ty.is_integer() {
                    result.ty
                } else {
                    // LHS is not an integer (e.g., both operands are literals),
                    // default to i32
                    Type::I32
                };
                (result, ty)
            }
        };

        // Now check RHS against the determined type
        let rhs_result = self.analyze_inst(air, rhs, TypeExpectation::Check(op_type), ctx)?;

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: op_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, op_type))
    }

    /// Analyze a comparison operator with bidirectional type inference.
    ///
    /// Synthesizes the type from the left operand in a single traversal, then checks
    /// the right operand against it. This eliminates the double traversal that was
    /// previously required by infer_type + analyze_inst.
    ///
    /// For equality operators (`==`, `!=`), both integers and booleans are allowed.
    /// For ordering operators (`<`, `>`, `<=`, `>=`), only integers are allowed.
    fn analyze_comparison<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        allow_bool: bool,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        // SINGLE TRAVERSAL: synthesize type AND emit AIR in one pass
        let lhs_result = self.analyze_inst(air, lhs, TypeExpectation::Synthesize, ctx)?;
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors
        if lhs_type.is_never() || lhs_type.is_error() {
            let rhs_result = self.analyze_inst(air, rhs, TypeExpectation::Check(Type::I32), ctx)?;
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::Bool,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Bool));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            if !lhs_type.is_integer() && lhs_type != Type::Bool {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer or bool".to_string(),
                        found: lhs_type.name().to_string(),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.name().to_string(),
                },
                self.rir.get(lhs).span,
            ));
        }

        // RHS is checked against synthesized LHS type
        let rhs_result = self.analyze_inst(air, rhs, TypeExpectation::Check(lhs_type), ctx)?;

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::Bool,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Bool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    fn compile_to_air(source: &str) -> CompileResult<SemaOutput> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize()?;
        let mut parser = Parser::new(tokens);
        let ast = parser.parse()?;

        let mut interner = Interner::new();
        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();

        let sema = Sema::new(&rir, &interner);
        sema.analyze_all()
    }

    #[test]
    fn test_analyze_simple_function() {
        let output = compile_to_air("fn main() -> i32 { 42 }").unwrap();
        let functions = &output.functions;

        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "main");

        let air = &functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        assert_eq!(air.len(), 2); // Const + Ret
    }

    #[test]
    fn test_analyze_addition() {
        let output = compile_to_air("fn main() -> i32 { 1 + 2 }").unwrap();

        let air = &output.functions[0].air;
        assert_eq!(air.return_type(), Type::I32);
        // Const(1) + Const(2) + Add + Ret = 4 instructions
        assert_eq!(air.len(), 4);

        // Check that add instruction exists with correct type
        let add_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(add_inst.data, AirInstData::Add(_, _)));
        assert_eq!(add_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_all_binary_ops() {
        // Test that all binary operators compile correctly
        assert!(compile_to_air("fn main() -> i32 { 1 + 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 - 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 * 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 / 2 }").is_ok());
        assert!(compile_to_air("fn main() -> i32 { 1 % 2 }").is_ok());
    }

    #[test]
    fn test_analyze_negation() {
        let output = compile_to_air("fn main() -> i32 { -42 }").unwrap();

        let air = &output.functions[0].air;
        // Const(42) + Neg + Ret = 3 instructions
        assert_eq!(air.len(), 3);

        let neg_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(neg_inst.data, AirInstData::Neg(_)));
        assert_eq!(neg_inst.ty, Type::I32);
    }

    #[test]
    fn test_analyze_complex_expr() {
        let output = compile_to_air("fn main() -> i32 { (1 + 2) * 3 }").unwrap();

        let air = &output.functions[0].air;
        // Const(1) + Const(2) + Add + Const(3) + Mul + Ret = 6 instructions
        assert_eq!(air.len(), 6);

        // Check that result is multiplication
        let mul_inst = air.get(AirRef::from_raw(4));
        assert!(matches!(mul_inst.data, AirInstData::Mul(_, _)));
    }

    #[test]
    fn test_analyze_let_binding() {
        let output = compile_to_air("fn main() -> i32 { let x = 42; x }").unwrap();

        assert_eq!(output.functions.len(), 1);
        assert_eq!(output.functions[0].num_locals, 1);

        let air = &output.functions[0].air;
        // Const(42) + Alloc + Load + Block([Alloc], Load) + Ret = 5 instructions
        assert_eq!(air.len(), 5);

        // Check alloc instruction
        let alloc_inst = air.get(AirRef::from_raw(1));
        assert!(matches!(
            alloc_inst.data,
            AirInstData::Alloc { slot: 0, .. }
        ));

        // Check load instruction
        let load_inst = air.get(AirRef::from_raw(2));
        assert!(matches!(load_inst.data, AirInstData::Load { slot: 0 }));

        // Check block instruction groups the alloc with the load
        let block_inst = air.get(AirRef::from_raw(3));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_analyze_let_mut_assignment() {
        let output = compile_to_air("fn main() -> i32 { let mut x = 10; x = 20; x }").unwrap();

        let air = &output.functions[0].air;
        // Const(10) + Alloc + Const(20) + Store + Load + Block([Alloc, Store], Load) + Ret = 7 instructions
        assert_eq!(air.len(), 7);

        // Check store instruction
        let store_inst = air.get(AirRef::from_raw(3));
        assert!(matches!(
            store_inst.data,
            AirInstData::Store { slot: 0, .. }
        ));

        // Check block instruction groups statements
        let block_inst = air.get(AirRef::from_raw(5));
        assert!(matches!(block_inst.data, AirInstData::Block { .. }));
    }

    #[test]
    fn test_undefined_variable() {
        let result = compile_to_air("fn main() -> i32 { x }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::UndefinedVariable(_)));
    }

    #[test]
    fn test_assign_to_immutable() {
        let result = compile_to_air("fn main() -> i32 { let x = 10; x = 20; x }");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::AssignToImmutable(_)));
    }

    #[test]
    fn test_multiple_variables() {
        let output = compile_to_air("fn main() -> i32 { let x = 10; let y = 20; x + y }").unwrap();

        assert_eq!(output.functions[0].num_locals, 2);
    }
}
