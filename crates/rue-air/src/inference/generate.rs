//! Constraint generation for Hindley-Milner type inference.
//!
//! This module provides the constraint generation phase (Phase 1 of HM inference):
//! - [`ConstraintContext`] - Scoped variable tracking during generation
//! - [`ExprInfo`] - Result of constraint generation for an expression
//! - [`ConstraintGenerator`] - Walks RIR and generates type constraints
//! - Function/method signature types for type checking

use super::constraint::Constraint;
use super::types::{InferType, TypeVarAllocator, TypeVarId};
use crate::Type;
use crate::scope::ScopedContext;
use crate::types::parse_array_type_syntax;
use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, Rir};
use rue_span::Span;
use std::collections::HashMap;

/// Information about a local variable during constraint generation.
#[derive(Debug, Clone)]
pub struct LocalVarInfo {
    /// The inferred type of this variable.
    pub ty: InferType,
    /// Whether the variable is mutable.
    pub is_mut: bool,
    /// Span of the variable declaration.
    pub span: Span,
}

/// Information about a function parameter during constraint generation.
#[derive(Debug, Clone)]
pub struct ParamVarInfo {
    /// The type of this parameter, as InferType for uniform handling.
    pub ty: InferType,
}

/// Information about a function during constraint generation.
///
/// Uses `InferType` rather than `Type` so that array types are represented
/// structurally (as `InferType::Array { element, length }`) rather than by
/// opaque IDs. This allows uniform handling during inference.
#[derive(Debug, Clone)]
pub struct FunctionSig {
    /// Parameter types (in order), as InferTypes for uniform handling.
    pub param_types: Vec<InferType>,
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
}

/// Information about a method during constraint generation.
///
/// Used for method calls (receiver.method()) and associated function calls (Type::function()).
#[derive(Debug, Clone)]
pub struct MethodSig {
    /// The struct type this method belongs to (as concrete Type::Struct)
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter types (excluding self), as InferTypes for uniform handling.
    pub param_types: Vec<InferType>,
    /// Return type, as InferType for uniform handling.
    pub return_type: InferType,
}

/// Context for constraint generation within a single function.
pub struct ConstraintContext<'a> {
    /// Local variables in scope.
    pub locals: HashMap<Spur, LocalVarInfo>,
    /// Function parameters.
    pub params: &'a HashMap<Spur, ParamVarInfo>,
    /// Return type of the current function.
    pub return_type: Type,
    /// How many loops we're nested inside (for break/continue validation).
    pub loop_depth: u32,
    /// Scope stack for efficient scope management.
    scope_stack: Vec<Vec<(Spur, Option<LocalVarInfo>)>>,
}

impl<'a> ConstraintContext<'a> {
    /// Create a new context for a function.
    pub fn new(params: &'a HashMap<Spur, ParamVarInfo>, return_type: Type) -> Self {
        Self {
            locals: HashMap::new(),
            params,
            return_type,
            loop_depth: 0,
            scope_stack: Vec::new(),
        }
    }
}

impl ScopedContext for ConstraintContext<'_> {
    type VarInfo = LocalVarInfo;

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
}

/// Result of constraint generation for an expression.
#[derive(Debug, Clone)]
pub struct ExprInfo {
    /// The inferred type of this expression.
    pub ty: InferType,
    /// The span of this expression (for error reporting).
    pub span: Span,
}

impl ExprInfo {
    /// Create a new expression info.
    pub fn new(ty: InferType, span: Span) -> Self {
        Self { ty, span }
    }
}

/// Constraint generator that walks RIR and generates type constraints.
///
/// This is Phase 1 of HM inference: constraint generation. The constraints
/// are later solved by the `Unifier` to determine concrete types.
pub struct ConstraintGenerator<'a> {
    /// The RIR being analyzed.
    rir: &'a Rir,
    /// String interner for resolving symbols.
    interner: &'a ThreadedRodeo,
    /// Type variable allocator.
    type_vars: TypeVarAllocator,
    /// Collected constraints.
    constraints: Vec<Constraint>,
    /// Mapping from RIR instruction to its inferred type.
    expr_types: HashMap<InstRef, InferType>,
    /// Function signatures (for call type checking).
    functions: &'a HashMap<Spur, FunctionSig>,
    /// Struct types (name -> Type::Struct(id)).
    structs: &'a HashMap<Spur, Type>,
    /// Enum types (name -> Type::Enum(id)).
    enums: &'a HashMap<Spur, Type>,
    /// Method signatures: (struct_name, method_name) -> MethodSig
    methods: &'a HashMap<(Spur, Spur), MethodSig>,
    /// Type variables allocated for integer literals.
    /// These start as unbound and need to be defaulted to i32 if unconstrained.
    int_literal_vars: Vec<TypeVarId>,
}

impl<'a> ConstraintGenerator<'a> {
    /// Create a new constraint generator.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        functions: &'a HashMap<Spur, FunctionSig>,
        structs: &'a HashMap<Spur, Type>,
        enums: &'a HashMap<Spur, Type>,
        methods: &'a HashMap<(Spur, Spur), MethodSig>,
    ) -> Self {
        Self {
            rir,
            interner,
            type_vars: TypeVarAllocator::new(),
            constraints: Vec::new(),
            expr_types: HashMap::new(),
            functions,
            structs,
            enums,
            methods,
            int_literal_vars: Vec::new(),
        }
    }

    /// Get the type variables allocated for integer literals.
    pub fn int_literal_vars(&self) -> &[TypeVarId] {
        &self.int_literal_vars
    }

    /// Allocate a fresh type variable.
    pub fn fresh_var(&mut self) -> TypeVarId {
        self.type_vars.fresh()
    }

    /// Add a constraint.
    pub fn add_constraint(&mut self, constraint: Constraint) {
        self.constraints.push(constraint);
    }

    /// Record the type of an expression.
    pub fn record_type(&mut self, inst_ref: InstRef, ty: InferType) {
        self.expr_types.insert(inst_ref, ty);
    }

    /// Get the recorded type of an expression.
    pub fn get_type(&self, inst_ref: InstRef) -> Option<&InferType> {
        self.expr_types.get(&inst_ref)
    }

    /// Get all collected constraints.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Take ownership of the collected constraints.
    pub fn take_constraints(self) -> Vec<Constraint> {
        self.constraints
    }

    /// Get the expression type mapping.
    pub fn expr_types(&self) -> &HashMap<InstRef, InferType> {
        &self.expr_types
    }

    /// Consume the constraint generator and return (constraints, int_literal_vars, expr_types, type_var_count).
    ///
    /// This is useful when you need ownership of the expression types map.
    /// The `type_var_count` can be used to pre-size the unifier's substitution for better performance.
    pub fn into_parts(
        self,
    ) -> (
        Vec<Constraint>,
        Vec<TypeVarId>,
        HashMap<InstRef, InferType>,
        u32,
    ) {
        (
            self.constraints,
            self.int_literal_vars,
            self.expr_types,
            self.type_vars.count(),
        )
    }

    /// Generate constraints for an expression.
    ///
    /// Returns the inferred type of the expression. Records the type in
    /// `expr_types` and adds constraints to `constraints`.
    pub fn generate(&mut self, inst_ref: InstRef, ctx: &mut ConstraintContext) -> ExprInfo {
        let inst = self.rir.get(inst_ref);
        let span = inst.span;

        let ty = match &inst.data {
            InstData::IntConst(_) => {
                // Integer literals get a fresh type variable that we immediately
                // bind to IntLiteral. This allows unification to track when the
                // literal is constrained to a specific integer type.
                //
                // Example: `let x: i64 = 42` generates:
                //   - type_var(?0) for the literal 42
                //   - substitution: ?0 -> IntLiteral
                //   - constraint: Equal(Var(?0), Concrete(i64))
                //
                // During unification, Equal(IntLiteral, Concrete(i64)) succeeds
                // and rebinds ?0 -> Concrete(i64) via rebind_int_literal_to_concrete.
                let var = self.fresh_var();
                self.int_literal_vars.push(var);
                InferType::Var(var)
            }

            InstData::BoolConst(_) => InferType::Concrete(Type::Bool),

            // String constants use the builtin String struct type.
            InstData::StringConst(_) => {
                // Look up the String type from the structs map
                if let Some(string_spur) = self.interner.get("String") {
                    if let Some(&string_ty) = self.structs.get(&string_spur) {
                        InferType::Concrete(string_ty)
                    } else {
                        // Fallback if String struct not found (shouldn't happen after builtin injection)
                        InferType::Concrete(Type::Error)
                    }
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            InstData::UnitConst => InferType::Concrete(Type::Unit),

            // Binary arithmetic: both operands must have the same type, result is that type
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs } => self.generate_binary_arith(*lhs, *rhs, ctx),

            // Bitwise operations: same as arithmetic
            InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => self.generate_binary_arith(*lhs, *rhs, ctx),

            // Comparison operators: operands must match, result is bool
            InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs } => {
                let lhs_info = self.generate(*lhs, ctx);
                let rhs_info = self.generate(*rhs, ctx);
                // Operands must have the same type
                self.add_constraint(Constraint::equal(lhs_info.ty, rhs_info.ty, span));
                InferType::Concrete(Type::Bool)
            }

            // Logical operators: operands must be bool, result is bool
            InstData::And { lhs, rhs } | InstData::Or { lhs, rhs } => {
                let lhs_info = self.generate(*lhs, ctx);
                let rhs_info = self.generate(*rhs, ctx);
                self.add_constraint(Constraint::equal(
                    lhs_info.ty,
                    InferType::Concrete(Type::Bool),
                    lhs_info.span,
                ));
                self.add_constraint(Constraint::equal(
                    rhs_info.ty,
                    InferType::Concrete(Type::Bool),
                    rhs_info.span,
                ));
                InferType::Concrete(Type::Bool)
            }

            // Unary negation: operand must be signed integer
            InstData::Neg { operand } => {
                let operand_info = self.generate(*operand, ctx);
                // Result type is the same as operand type
                let result_ty = operand_info.ty.clone();
                // Must be a signed integer
                self.add_constraint(Constraint::is_signed(result_ty.clone(), span));
                result_ty
            }

            // Logical NOT: operand must be bool
            InstData::Not { operand } => {
                let operand_info = self.generate(*operand, ctx);
                self.add_constraint(Constraint::equal(
                    operand_info.ty,
                    InferType::Concrete(Type::Bool),
                    operand_info.span,
                ));
                InferType::Concrete(Type::Bool)
            }

            // Bitwise NOT: operand must be integer
            InstData::BitNot { operand } => {
                let operand_info = self.generate(*operand, ctx);
                let result_ty = operand_info.ty.clone();
                // Must be an integer type (signed or unsigned)
                self.add_constraint(Constraint::is_integer(result_ty.clone(), span));
                result_ty
            }

            // Variable reference
            InstData::VarRef { name } => {
                if let Some(local) = ctx.locals.get(name) {
                    local.ty.clone()
                } else if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else {
                    // Unknown variable - will be caught during semantic analysis
                    InferType::Concrete(Type::Error)
                }
            }

            // Parameter reference
            InstData::ParamRef { name, .. } => {
                if let Some(param) = ctx.params.get(name) {
                    param.ty.clone()
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            // Local variable allocation
            InstData::Alloc {
                directives_start: _,
                directives_len: _,
                name,
                is_mut,
                ty: type_annotation,
                init,
            } => {
                let init_info = self.generate(*init, ctx);

                let var_ty = if let Some(ty_sym) = type_annotation {
                    // Explicit type annotation - use it and constrain init to match
                    let ty_name = self.interner.resolve(ty_sym);
                    if let Some(annotated_ty) = self.resolve_type_name(ty_name) {
                        self.add_constraint(Constraint::equal(
                            init_info.ty,
                            annotated_ty.clone(),
                            span,
                        ));
                        annotated_ty
                    } else {
                        // Unknown type name (e.g., struct/enum) - use init type for now.
                        // Semantic analysis will catch undefined types and verify struct/enum
                        // field types match the definition.
                        init_info.ty
                    }
                } else {
                    // No annotation - use the init expression's type
                    init_info.ty
                };

                // Record the variable in scope (if it has a name)
                if let Some(var_name) = name {
                    ctx.insert_local(
                        *var_name,
                        LocalVarInfo {
                            ty: var_ty.clone(),
                            is_mut: *is_mut,
                            span,
                        },
                    );
                }

                // Alloc produces unit type
                InferType::Concrete(Type::Unit)
            }

            // Assignment
            InstData::Assign { name, value } => {
                let value_info = self.generate(*value, ctx);
                if let Some(local) = ctx.locals.get(name) {
                    // Constrain value to match variable type
                    self.add_constraint(Constraint::equal(value_info.ty, local.ty.clone(), span));
                }
                // Assignment produces unit
                InferType::Concrete(Type::Unit)
            }

            // Return statement
            InstData::Ret(value) => {
                if let Some(val_ref) = value {
                    let value_info = self.generate(*val_ref, ctx);
                    // Constrain return value to match function return type
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                } else {
                    // Return without value - function must return unit
                    self.add_constraint(Constraint::equal(
                        InferType::Concrete(Type::Unit),
                        InferType::Concrete(ctx.return_type),
                        span,
                    ));
                }
                // Return diverges
                InferType::Concrete(Type::Never)
            }

            // Function call
            InstData::Call {
                name,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);
                if let Some(func) = self.functions.get(name) {
                    // Check argument count matches parameter count.
                    // Semantic analysis will emit a proper error; we just need to avoid
                    // panicking and process what we can.
                    if args.len() != func.param_types.len() {
                        // Still process all arguments to catch type errors within them
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        // Return the declared return type (error will be caught in sema)
                        func.return_type.clone()
                    } else {
                        // Generate constraints for each argument
                        for (arg, param_ty) in args.iter().zip(func.param_types.iter()) {
                            let arg_info = self.generate(arg.value, ctx);
                            self.add_constraint(Constraint::equal(
                                arg_info.ty,
                                param_ty.clone(),
                                arg_info.span,
                            ));
                        }
                        func.return_type.clone()
                    }
                } else {
                    // Unknown function - still process arguments for constraint generation
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Concrete(Type::Error)
                }
            }

            // Intrinsic call
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                let intrinsic_name = self.interner.resolve(name);
                let args = self.rir.get_inst_refs(*args_start, *args_len);

                if intrinsic_name == "intCast" {
                    // @intCast: target type is inferred from context
                    // The argument must be an integer type
                    if !args.is_empty() {
                        let arg_info = self.generate(args[0], ctx);
                        // Constraint: argument must be an integer
                        // We'll check this in sema, for now just process the argument
                        let _ = arg_info;
                    }
                    // Return type is inferred from context - create a fresh type variable
                    let result_var = self.fresh_var();
                    InferType::Var(result_var)
                } else if intrinsic_name == "read_line" {
                    // @read_line: returns String (same as string constants)
                    if let Some(string_spur) = self.interner.get("String") {
                        if let Some(&string_ty) = self.structs.get(&string_spur) {
                            InferType::Concrete(string_ty)
                        } else {
                            // Fallback if String struct not found
                            InferType::Concrete(Type::Error)
                        }
                    } else {
                        InferType::Concrete(Type::Error)
                    }
                } else if intrinsic_name == "parse_i32" {
                    // @parse_i32: takes a String, returns i32
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::I32)
                } else if intrinsic_name == "parse_i64" {
                    // @parse_i64: takes a String, returns i64
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::I64)
                } else if intrinsic_name == "parse_u32" {
                    // @parse_u32: takes a String, returns u32
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::U32)
                } else if intrinsic_name == "parse_u64" {
                    // @parse_u64: takes a String, returns u64
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    InferType::Concrete(Type::U64)
                } else {
                    // Generate constraints for arguments (they need to be processed)
                    for arg_ref in args.iter() {
                        self.generate(*arg_ref, ctx);
                    }
                    // @dbg and other intrinsics return Unit
                    InferType::Concrete(Type::Unit)
                }
            }

            // Type intrinsic (@size_of, @align_of)
            InstData::TypeIntrinsic {
                name: _,
                type_arg: _,
            } => {
                // Type intrinsics return i32 (the size or alignment value)
                InferType::Concrete(Type::I32)
            }

            // Block
            InstData::Block { extra_start, len } => {
                ctx.push_scope();
                let mut last_ty = InferType::Concrete(Type::Unit);
                let block_insts = self.rir.get_extra(*extra_start, *len);
                for &inst_raw in block_insts {
                    let block_inst_ref = InstRef::from_raw(inst_raw);
                    let info = self.generate(block_inst_ref, ctx);
                    last_ty = info.ty;
                }
                ctx.pop_scope();
                last_ty
            }

            // Branch (if/else)
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::Bool),
                    cond_info.span,
                ));

                let then_info = self.generate(*then_block, ctx);

                if let Some(else_ref) = else_block {
                    let else_info = self.generate(*else_ref, ctx);

                    // Handle Never type coercion:
                    // - If one branch is Never, the if-else takes the other branch's type
                    // - If both are Never, the result is Never
                    // - Otherwise, both must unify to the same type
                    let then_is_never = matches!(&then_info.ty, InferType::Concrete(Type::Never));
                    let else_is_never = matches!(&else_info.ty, InferType::Concrete(Type::Never));

                    match (then_is_never, else_is_never) {
                        (true, true) => {
                            // Both diverge - result is Never
                            InferType::Concrete(Type::Never)
                        }
                        (true, false) => {
                            // Then diverges - result is else type
                            else_info.ty
                        }
                        (false, true) => {
                            // Else diverges - result is then type
                            then_info.ty
                        }
                        (false, false) => {
                            // Neither diverges - both must have the same type
                            let result_var = self.fresh_var();
                            let result_ty = InferType::Var(result_var);
                            self.add_constraint(Constraint::equal(
                                then_info.ty,
                                result_ty.clone(),
                                then_info.span,
                            ));
                            self.add_constraint(Constraint::equal(
                                else_info.ty,
                                result_ty.clone(),
                                else_info.span,
                            ));
                            result_ty
                        }
                    }
                } else {
                    // No else branch - the if expression has unit type
                    // (or the then branch type if it's unit-compatible)
                    InferType::Concrete(Type::Unit)
                }
            }

            // While loop
            InstData::Loop { cond, body } => {
                let cond_info = self.generate(*cond, ctx);
                self.add_constraint(Constraint::equal(
                    cond_info.ty,
                    InferType::Concrete(Type::Bool),
                    cond_info.span,
                ));

                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // Loops produce unit
                InferType::Concrete(Type::Unit)
            }

            // Infinite loop
            InstData::InfiniteLoop { body } => {
                ctx.loop_depth += 1;
                self.generate(*body, ctx);
                ctx.loop_depth -= 1;

                // Infinite loop without break never returns
                InferType::Concrete(Type::Never)
            }

            // Break/Continue
            InstData::Break | InstData::Continue => InferType::Concrete(Type::Never),

            // Match expression
            InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            } => {
                let scrutinee_info = self.generate(*scrutinee, ctx);
                let arms = self.rir.get_match_arms(*arms_start, *arms_len);

                // Collect arm types, handling Never coercion
                let mut arm_types: Vec<ExprInfo> = Vec::new();
                for (pattern, body) in arms.iter() {
                    // Patterns constrain the scrutinee type
                    let pattern_ty = self.pattern_type(pattern);
                    self.add_constraint(Constraint::equal(
                        scrutinee_info.ty.clone(),
                        pattern_ty,
                        pattern.span(),
                    ));

                    // Generate body and collect its type
                    let body_info = self.generate(*body, ctx);
                    arm_types.push(body_info);
                }

                // Handle Never type coercion:
                // Filter out Never arms and use the remaining non-Never types
                let non_never_arms: Vec<_> = arm_types
                    .iter()
                    .filter(|info| !matches!(&info.ty, InferType::Concrete(Type::Never)))
                    .collect();

                if non_never_arms.is_empty() {
                    // All arms diverge - result is Never
                    InferType::Concrete(Type::Never)
                } else {
                    // Create constraints for non-Never arms to have the same type
                    let result_var = self.fresh_var();
                    let result_ty = InferType::Var(result_var);
                    for arm_info in non_never_arms {
                        self.add_constraint(Constraint::equal(
                            arm_info.ty.clone(),
                            result_ty.clone(),
                            arm_info.span,
                        ));
                    }
                    result_ty
                }
            }

            // Struct initialization
            InstData::StructInit {
                type_name,
                fields_start,
                fields_len,
            } => {
                if let Some(&struct_ty) = self.structs.get(type_name) {
                    let fields = self.rir.get_field_inits(*fields_start, *fields_len);
                    // Generate constraints for each field
                    for (_, value_ref) in fields.iter() {
                        self.generate(*value_ref, ctx);
                    }
                    InferType::Concrete(struct_ty)
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            // Field access
            InstData::FieldGet { base, field: _ } => {
                // Generate constraints for the base expression (needed for nested field access)
                let _base_info = self.generate(*base, ctx);
                // We need to look up the field type from the struct definition.
                // For now, use a fresh type variable - full resolution happens during
                // semantic analysis which has access to struct definitions.
                let result_var = self.fresh_var();
                InferType::Var(result_var)
            }

            // Field assignment
            InstData::FieldSet {
                base,
                field: _,
                value,
            } => {
                self.generate(*base, ctx);
                self.generate(*value, ctx);
                InferType::Concrete(Type::Unit)
            }

            // Enum variant
            InstData::EnumVariant {
                type_name,
                variant: _,
            } => {
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::Error)
                }
            }

            // Array initialization
            InstData::ArrayInit {
                elems_start,
                elems_len,
            } => {
                let elements = self.rir.get_inst_refs(*elems_start, *elems_len);
                if elements.is_empty() {
                    // Empty array - need type annotation to know element type
                    // Use a fresh type variable for the element type
                    let elem_var = self.fresh_var();
                    InferType::Array {
                        element: Box::new(InferType::Var(elem_var)),
                        length: 0,
                    }
                } else {
                    // Get element type from first element, constrain rest to match
                    let first_info = self.generate(elements[0], ctx);
                    for elem_ref in elements.iter().skip(1) {
                        let elem_info = self.generate(*elem_ref, ctx);
                        self.add_constraint(Constraint::equal(
                            elem_info.ty,
                            first_info.ty.clone(),
                            elem_info.span,
                        ));
                    }
                    // Build the array type with the inferred element type
                    InferType::Array {
                        element: Box::new(first_info.ty),
                        length: elements.len() as u64,
                    }
                }
            }

            // Array index
            InstData::IndexGet { base, index } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                // Index must be an unsigned integer type
                self.add_constraint(Constraint::is_unsigned(index_info.ty, index_info.span));

                // Extract element type from array type.
                // If base is InferType::Array, we can get the element type directly.
                // Otherwise, we need a fresh variable that will be resolved later.
                match &base_info.ty {
                    InferType::Array { element, .. } => (**element).clone(),
                    _ => {
                        // Base might be a type variable that will resolve to an array.
                        // Use a fresh variable for the element type.
                        let result_var = self.fresh_var();
                        InferType::Var(result_var)
                    }
                }
            }

            // Array index assignment
            InstData::IndexSet { base, index, value } => {
                let base_info = self.generate(*base, ctx);
                let index_info = self.generate(*index, ctx);
                // Index must be an unsigned integer type
                self.add_constraint(Constraint::is_unsigned(index_info.ty, index_info.span));

                let value_info = self.generate(*value, ctx);

                // Constrain value type to match array element type
                if let InferType::Array { element, .. } = &base_info.ty {
                    self.add_constraint(Constraint::equal(
                        value_info.ty,
                        (**element).clone(),
                        value_info.span,
                    ));
                }

                InferType::Concrete(Type::Unit)
            }

            // Type declarations don't produce values
            InstData::FnDecl { .. }
            | InstData::StructDecl { .. }
            | InstData::EnumDecl { .. }
            | InstData::ImplDecl { .. }
            | InstData::DropFnDecl { .. } => InferType::Concrete(Type::Unit),

            // Method call: receiver.method(args)
            InstData::MethodCall {
                receiver,
                method,
                args_start,
                args_len,
            } => {
                // Generate type for receiver
                let receiver_info = self.generate(*receiver, ctx);
                let args = self.rir.get_call_args(*args_start, *args_len);

                // Get struct name from receiver type if it's a struct
                // If we can't determine the struct type, we still generate constraints
                // for the arguments and return a type variable (actual error is in sema)
                let result_type = if let InferType::Concrete(Type::Struct(struct_id)) =
                    &receiver_info.ty
                {
                    // Find the struct name symbol
                    let struct_name = self
                        .structs
                        .iter()
                        .find(|(_, ty)| **ty == Type::Struct(*struct_id))
                        .map(|(name, _)| *name);

                    if let Some(struct_name) = struct_name {
                        let method_key = (struct_name, *method);
                        if let Some(method_sig) = self.methods.get(&method_key) {
                            // Generate constraints for arguments
                            for (arg, param_type) in args.iter().zip(method_sig.param_types.iter())
                            {
                                let arg_info = self.generate(arg.value, ctx);
                                self.add_constraint(Constraint::equal(
                                    arg_info.ty,
                                    param_type.clone(),
                                    arg_info.span,
                                ));
                            }
                            method_sig.return_type.clone()
                        } else {
                            // Method not found - sema will report the error
                            // Still generate arg types to catch errors in arguments
                            for arg in args.iter() {
                                self.generate(arg.value, ctx);
                            }
                            InferType::Concrete(Type::Error)
                        }
                    } else {
                        // Couldn't find struct name - shouldn't happen but handle gracefully
                        for arg in args.iter() {
                            self.generate(arg.value, ctx);
                        }
                        InferType::Concrete(Type::Error)
                    }
                } else {
                    // Non-struct receiver - sema will report the error
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Concrete(Type::Error)
                };

                result_type
            }

            // Associated function call: Type::function(args)
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                let args = self.rir.get_call_args(*args_start, *args_len);
                let method_key = (*type_name, *function);
                if let Some(method_sig) = self.methods.get(&method_key) {
                    // Generate constraints for arguments
                    for (arg, param_type) in args.iter().zip(method_sig.param_types.iter()) {
                        let arg_info = self.generate(arg.value, ctx);
                        self.add_constraint(Constraint::equal(
                            arg_info.ty,
                            param_type.clone(),
                            arg_info.span,
                        ));
                    }
                    method_sig.return_type.clone()
                } else {
                    // Method not found - sema will report the error
                    // Still generate arg types to catch errors in arguments
                    for arg in args.iter() {
                        self.generate(arg.value, ctx);
                    }
                    InferType::Concrete(Type::Error)
                }
            }

            // Comptime block: the type depends on whether evaluation succeeds at compile time.
            // For type inference, we use a fresh type variable that can unify with
            // whatever type is expected from the context (e.g., a let binding's type annotation).
            // Similar to integer literals, comptime blocks can adapt to their context.
            InstData::Comptime { expr } => {
                // Generate constraints for the inner expression
                let inner_info = self.generate(*expr, ctx);

                // Use a fresh variable so comptime can unify with expected type from context.
                // The actual evaluation happens in sema where we know the final type.
                let var = self.fresh_var();
                self.int_literal_vars.push(var);
                // Add constraint that this var equals the inner expression's type
                self.add_constraint(Constraint::equal(InferType::Var(var), inner_info.ty, span));
                InferType::Var(var)
            }
        };

        // Record the type for this expression
        self.record_type(inst_ref, ty.clone());
        ExprInfo::new(ty, span)
    }

    /// Generate constraints for a binary arithmetic operation.
    fn generate_binary_arith(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        ctx: &mut ConstraintContext,
    ) -> InferType {
        let lhs_info = self.generate(lhs, ctx);
        let rhs_info = self.generate(rhs, ctx);

        // Both operands must have the same type
        // Use a fresh type variable for the result
        let result_var = self.fresh_var();
        let result_ty = InferType::Var(result_var);

        self.add_constraint(Constraint::equal(
            lhs_info.ty,
            result_ty.clone(),
            lhs_info.span,
        ));
        self.add_constraint(Constraint::equal(
            rhs_info.ty,
            result_ty.clone(),
            rhs_info.span,
        ));

        // Result must be an integer type (catches errors like `true + 1` early)
        self.add_constraint(Constraint::is_integer(result_ty.clone(), lhs_info.span));

        result_ty
    }

    /// Get the inferred type for a pattern.
    fn pattern_type(&mut self, pattern: &rue_rir::RirPattern) -> InferType {
        match pattern {
            rue_rir::RirPattern::Wildcard(_) => {
                // Wildcard matches anything - use a fresh type variable
                let var = self.fresh_var();
                InferType::Var(var)
            }
            rue_rir::RirPattern::Int(_, _) => InferType::IntLiteral,
            rue_rir::RirPattern::Bool(_, _) => InferType::Concrete(Type::Bool),
            rue_rir::RirPattern::Path { type_name, .. } => {
                if let Some(&enum_ty) = self.enums.get(type_name) {
                    InferType::Concrete(enum_ty)
                } else {
                    InferType::Concrete(Type::Error)
                }
            }
        }
    }

    /// Resolve a type name to an InferType.
    ///
    /// Handles primitive types, array syntax `[T; N]`, and struct/enum types.
    fn resolve_type_name(&self, name: &str) -> Option<InferType> {
        // Check for array syntax first: [T; N]
        if let Some((element_type_str, length)) = parse_array_type_syntax(name) {
            // Recursively resolve the element type
            let element_ty = self.resolve_type_name(&element_type_str)?;
            return Some(InferType::Array {
                element: Box::new(element_ty),
                length,
            });
        }

        // Check primitives
        let ty = match name {
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "bool" => Type::Bool,
            "()" => Type::Unit,
            _ => {
                // Check for struct types (including builtin String)
                if let Some(name_spur) = self.interner.get(name) {
                    if let Some(&struct_ty) = self.structs.get(&name_spur) {
                        return Some(InferType::Concrete(struct_ty));
                    }
                    if let Some(&enum_ty) = self.enums.get(&name_spur) {
                        return Some(InferType::Concrete(enum_ty));
                    }
                }
                return None;
            }
        };
        Some(InferType::Concrete(ty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    /// Helper to create a minimal RIR for testing.
    fn make_test_rir_and_interner() -> (Rir, ThreadedRodeo) {
        let rir = Rir::new();
        let interner = ThreadedRodeo::new();
        (rir, interner)
    }

    #[test]
    fn test_constraint_generator_int_literal() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Add an integer constant to RIR
        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(0, 2),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(inst_ref, &mut ctx);

        // Integer literals now get a type variable (tracked as int literal var)
        assert!(matches!(info.ty, InferType::Var(_)));
        // The type variable should be tracked in int_literal_vars
        assert_eq!(cgen.int_literal_vars().len(), 1);
        // No constraints should be generated for a simple literal
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bool_literal() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        let inst_ref = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(inst_ref, &mut ctx);

        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_binary_add() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: 1 + 2
        let lhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(4, 5),
        });
        let add = rir.add_inst(rue_rir::Inst {
            data: InstData::Add { lhs, rhs },
            span: Span::new(0, 5),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(add, &mut ctx);

        // Result should be a type variable
        assert!(info.ty.is_var());
        // Should generate 3 constraints: lhs = result, rhs = result, IsInteger(result)
        assert_eq!(cgen.constraints().len(), 3);
        // Verify the third constraint is IsInteger
        match &cgen.constraints()[2] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint for arithmetic result"),
        }
    }

    #[test]
    fn test_constraint_generator_comparison() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: 1 < 2
        let lhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(0, 1),
        });
        let rhs = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(4, 5),
        });
        let lt = rir.add_inst(rue_rir::Inst {
            data: InstData::Lt { lhs, rhs },
            span: Span::new(0, 5),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(lt, &mut ctx);

        // Comparisons always return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        // Should generate 1 constraint: lhs type = rhs type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_logical_and() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: true && false
        let lhs = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 4),
        });
        let rhs = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(false),
            span: Span::new(8, 13),
        });
        let and = rir.add_inst(rue_rir::Inst {
            data: InstData::And { lhs, rhs },
            span: Span::new(0, 13),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(and, &mut ctx);

        // Logical operators return Bool
        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        // Should generate 2 constraints: lhs = bool, rhs = bool
        assert_eq!(cgen.constraints().len(), 2);
    }

    #[test]
    fn test_constraint_generator_negation() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: -42
        let operand = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let neg = rir.add_inst(rue_rir::Inst {
            data: InstData::Neg { operand },
            span: Span::new(0, 3),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(neg, &mut ctx);

        // Negation preserves the operand type (now a type variable for the int literal)
        assert!(matches!(info.ty, InferType::Var(_)));
        // Should generate 1 constraint: IsSigned for the result
        assert_eq!(cgen.constraints().len(), 1);
        // Verify it's an IsSigned constraint
        match &cgen.constraints()[0] {
            Constraint::IsSigned(_, _) => {}
            _ => panic!("Expected IsSigned constraint"),
        }
    }

    #[test]
    fn test_constraint_generator_return() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: return 42
        let value = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(7, 9),
        });
        let ret = rir.add_inst(rue_rir::Inst {
            data: InstData::Ret(Some(value)),
            span: Span::new(0, 9),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(ret, &mut ctx);

        // Return is divergent (Never type)
        assert_eq!(info.ty, InferType::Concrete(Type::Never));
        // Should generate 1 constraint: return value = return type
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_generator_if_else() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: if true { 1 } else { 2 }
        let cond = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(3, 7),
        });
        let then_val = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(1),
            span: Span::new(10, 11),
        });
        let else_val = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(20, 21),
        });
        let branch = rir.add_inst(rue_rir::Inst {
            data: InstData::Branch {
                cond,
                then_block: then_val,
                else_block: Some(else_val),
            },
            span: Span::new(0, 25),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(branch, &mut ctx);

        // Result should be a type variable (unified from both branches)
        assert!(info.ty.is_var());
        // Should generate 3 constraints: cond = bool, then = result, else = result
        assert_eq!(cgen.constraints().len(), 3);
    }

    #[test]
    fn test_constraint_generator_while_loop() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: while true { 0 }
        let cond = rir.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(6, 10),
        });
        let body = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(13, 14),
        });
        let loop_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Loop { cond, body },
            span: Span::new(0, 15),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(loop_inst, &mut ctx);

        // While loops produce Unit
        assert_eq!(info.ty, InferType::Concrete(Type::Unit));
        // Should generate 1 constraint: cond = bool
        assert_eq!(cgen.constraints().len(), 1);
    }

    #[test]
    fn test_constraint_context_scope() {
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        // Use an interner to create a symbol
        let interner = ThreadedRodeo::new();
        let sym = interner.get_or_intern("x");
        ctx.insert_local(
            sym,
            LocalVarInfo {
                ty: InferType::Concrete(Type::I32),
                is_mut: false,
                span: Span::new(0, 1),
            },
        );

        assert!(ctx.locals.contains_key(&sym));

        // Push a scope and shadow the variable
        ctx.push_scope();
        ctx.insert_local(
            sym,
            LocalVarInfo {
                ty: InferType::Concrete(Type::I64),
                is_mut: true,
                span: Span::new(10, 15),
            },
        );

        // Should see the shadowed version
        let local = ctx.locals.get(&sym).unwrap();
        assert_eq!(local.ty, InferType::Concrete(Type::I64));
        assert!(local.is_mut);

        // Pop scope - should restore original
        ctx.pop_scope();
        let local = ctx.locals.get(&sym).unwrap();
        assert_eq!(local.ty, InferType::Concrete(Type::I32));
        assert!(!local.is_mut);
    }

    #[test]
    fn test_expr_info_creation() {
        let info = ExprInfo::new(InferType::IntLiteral, Span::new(5, 10));
        assert!(info.ty.is_int_literal());
        assert_eq!(info.span, Span::new(5, 10));
    }

    #[test]
    fn test_function_sig() {
        let sig = FunctionSig {
            param_types: vec![
                InferType::Concrete(Type::I32),
                InferType::Concrete(Type::Bool),
            ],
            return_type: InferType::Concrete(Type::I64),
        };
        assert_eq!(sig.param_types.len(), 2);
        assert_eq!(sig.return_type, InferType::Concrete(Type::I64));
    }

    #[test]
    fn test_constraint_generator_infinite_loop() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: loop { 0 }
        let body = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(6, 7),
        });
        let loop_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::InfiniteLoop { body },
            span: Span::new(0, 10),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(loop_inst, &mut ctx);

        // Infinite loop produces Never (diverges)
        assert_eq!(info.ty, InferType::Concrete(Type::Never));
        // No constraints for infinite loop itself
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_break_continue() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        let break_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Break,
            span: Span::new(0, 5),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(break_inst, &mut ctx);

        // Break diverges
        assert_eq!(info.ty, InferType::Concrete(Type::Never));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_index_get() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: arr[0]
        let base = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0), // Placeholder for array
            span: Span::new(0, 3),
        });
        let index = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(4, 5),
        });
        let index_get = rir.add_inst(rue_rir::Inst {
            data: InstData::IndexGet { base, index },
            span: Span::new(0, 6),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(index_get, &mut ctx);

        // Result is a type variable (element type unknown)
        assert!(info.ty.is_var());
        // Should generate 1 constraint: index must be unsigned
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsUnsigned(_, _) => {}
            _ => panic!("Expected IsUnsigned constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_index_set() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: arr[0] = 42
        let base = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0), // Placeholder for array
            span: Span::new(0, 3),
        });
        let index = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(0),
            span: Span::new(4, 5),
        });
        let value = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(9, 11),
        });
        let index_set = rir.add_inst(rue_rir::Inst {
            data: InstData::IndexSet { base, index, value },
            span: Span::new(0, 11),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(index_set, &mut ctx);

        // Index assignment produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::Unit));
        // Should generate 1 constraint: index must be unsigned
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsUnsigned(_, _) => {}
            _ => panic!("Expected IsUnsigned constraint for index"),
        }
    }

    #[test]
    fn test_constraint_generator_empty_block() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: { } (empty block)
        let block = rir.add_inst(rue_rir::Inst {
            data: InstData::Block {
                extra_start: 0,
                len: 0,
            },
            span: Span::new(0, 2),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Unit);

        let info = cgen.generate(block, &mut ctx);

        // Empty block produces Unit
        assert_eq!(info.ty, InferType::Concrete(Type::Unit));
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_bitwise_not() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: !42 (bitwise NOT)
        let operand = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(1, 3),
        });
        let bitnot = rir.add_inst(rue_rir::Inst {
            data: InstData::BitNot { operand },
            span: Span::new(0, 3),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(bitnot, &mut ctx);

        // Bitwise NOT preserves the operand type (now a type variable for int literal)
        assert!(matches!(info.ty, InferType::Var(_)));
        // Should generate 1 constraint: IsInteger for the result
        assert_eq!(cgen.constraints().len(), 1);
        match &cgen.constraints()[0] {
            Constraint::IsInteger(_, _) => {}
            _ => panic!("Expected IsInteger constraint"),
        }
    }

    #[test]
    fn test_constraint_generator_function_call_arg_count_mismatch() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let mut functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Register a function that takes 2 parameters
        let func_name = interner.get_or_intern("foo");
        functions.insert(
            func_name,
            FunctionSig {
                param_types: vec![
                    InferType::Concrete(Type::I32),
                    InferType::Concrete(Type::I32),
                ],
                return_type: InferType::Concrete(Type::Bool),
            },
        );

        // Create a call with only 1 argument (mismatch)
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(4, 6),
        });
        let (args_start, args_len) = rir.add_call_args(&[rue_rir::RirCallArg {
            value: arg,
            mode: rue_rir::RirArgMode::Normal,
        }]);
        let call = rir.add_inst(rue_rir::Inst {
            data: InstData::Call {
                name: func_name,
                args_start,
                args_len,
            },
            span: Span::new(0, 7),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::Bool);

        let info = cgen.generate(call, &mut ctx);

        // Should still return the declared return type
        assert_eq!(info.ty, InferType::Concrete(Type::Bool));
        // No constraints generated when arg count mismatches (error will be in sema)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_unknown_function() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new(); // Empty - no functions registered
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create a call to an unknown function
        let unknown_func = interner.get_or_intern("unknown");
        let arg = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(42),
            span: Span::new(8, 10),
        });
        let (args_start, args_len) = rir.add_call_args(&[rue_rir::RirCallArg {
            value: arg,
            mode: rue_rir::RirArgMode::Normal,
        }]);
        let call = rir.add_inst(rue_rir::Inst {
            data: InstData::Call {
                name: unknown_func,
                args_start,
                args_len,
            },
            span: Span::new(0, 11),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(call, &mut ctx);

        // Unknown function returns Error type
        assert_eq!(info.ty, InferType::Concrete(Type::Error));
        // Arguments should still be processed (but no constraints generated for them)
        assert_eq!(cgen.constraints().len(), 0);
    }

    #[test]
    fn test_constraint_generator_match_multiple_arms() {
        let (mut rir, interner) = make_test_rir_and_interner();
        let functions = HashMap::new();
        let structs = HashMap::new();
        let enums = HashMap::new();
        let methods: HashMap<(Spur, Spur), MethodSig> = HashMap::new();

        // Create: match x { 1 => 10, 2 => 20, _ => 30 }
        let scrutinee = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(5),
            span: Span::new(6, 7),
        });

        // Arm 1: 1 => 10
        let body1 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(10),
            span: Span::new(15, 17),
        });
        let pattern1 = rue_rir::RirPattern::Int(1, Span::new(10, 11));

        // Arm 2: 2 => 20
        let body2 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(20),
            span: Span::new(25, 27),
        });
        let pattern2 = rue_rir::RirPattern::Int(2, Span::new(20, 21));

        // Arm 3: _ => 30
        let body3 = rir.add_inst(rue_rir::Inst {
            data: InstData::IntConst(30),
            span: Span::new(35, 37),
        });
        let pattern3 = rue_rir::RirPattern::Wildcard(Span::new(30, 31));

        let arms = vec![(pattern1, body1), (pattern2, body2), (pattern3, body3)];
        let (arms_start, arms_len) = rir.add_match_arms(&arms);
        let match_inst = rir.add_inst(rue_rir::Inst {
            data: InstData::Match {
                scrutinee,
                arms_start,
                arms_len,
            },
            span: Span::new(0, 40),
        });

        let mut cgen =
            ConstraintGenerator::new(&rir, &interner, &functions, &structs, &enums, &methods);
        let params = HashMap::new();
        let mut ctx = ConstraintContext::new(&params, Type::I32);

        let info = cgen.generate(match_inst, &mut ctx);

        // Result should be a type variable (unified from all arm bodies)
        assert!(info.ty.is_var());

        // Should generate 6 constraints:
        // - 3 for pattern types matching scrutinee type (each arm)
        // - 3 for body types matching result type (each arm)
        assert_eq!(cgen.constraints().len(), 6);

        // Verify all constraints are Equal constraints
        for constraint in cgen.constraints() {
            match constraint {
                Constraint::Equal(_, _, _) => {}
                _ => panic!("Expected Equal constraint in match"),
            }
        }
    }
}
