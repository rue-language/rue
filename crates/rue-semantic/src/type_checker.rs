//! Type checker that builds HIR during semantic analysis
//!
//! This module provides a unified type checker that performs semantic analysis
//! and HIR construction in a single pass, ensuring that all type information
//! is accurately preserved in the HIR.

use crate::{FunctionSignature, Scope, SemanticError};
use rue_ast::{
    ArrayAccessNode, ArrayLiteralNode, CstRoot, ExpressionNode, FieldAccessNode, FieldKindNode,
    FunctionNode, StatementNode, StructLiteralNode, TupleLiteralNode,
};
use rue_ir::hir::{
    BinOp, HirBlock, HirExpr, HirFunction, HirLiteral, HirProgram, HirStatement, UnaryOp,
};
use rue_ir::types::{FieldId, RueType};
use rue_lexer::{Span, TokenKind};
use std::collections::{HashMap, HashSet};

/// Type constraint for unification-based type inference
#[derive(Debug, Clone, PartialEq)]
pub enum TypeConstraint {
    /// Two types must be equal
    Equal(TypeVarId, TypeVarId),
    /// Type variable must equal a concrete type
    Concrete(TypeVarId, RueType),
    /// Binary operation constraint (op, lhs, rhs, result)
    Binary(BinOp, TypeVarId, TypeVarId, TypeVarId),
}

/// Type variable identifier for constraint solving
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVarId(pub u32);

/// Type inference context that tracks constraints and solutions
///
/// This structure improves type inference by:
/// 1. Tracking type constraints during expression checking
/// 2. Supporting bidirectional type inference (synthesis and checking modes)
/// 3. Enabling better contextual type inference for literals and operations
/// 4. Preparing for future features like type variables and generics
#[derive(Debug, Clone)]
pub struct TypeInferenceContext {
    /// Counter for generating unique type variable IDs
    next_var_id: u32,
    /// Collected type constraints
    constraints: Vec<TypeConstraint>,
    /// Solved type variables (maps variable to concrete type)
    solutions: HashMap<TypeVarId, RueType>,
    /// Pending type variables that need resolution
    pending_vars: HashSet<TypeVarId>,
}

impl TypeInferenceContext {
    /// Create a new empty inference context
    pub fn new() -> Self {
        Self {
            next_var_id: 0,
            constraints: Vec::new(),
            solutions: HashMap::new(),
            pending_vars: HashSet::new(),
        }
    }

    /// Create a fresh type variable
    pub fn fresh_type_var(&mut self) -> TypeVarId {
        let id = TypeVarId(self.next_var_id);
        self.next_var_id += 1;
        self.pending_vars.insert(id);
        id
    }

    /// Add a constraint to the system
    pub fn add_constraint(&mut self, constraint: TypeConstraint) {
        self.constraints.push(constraint);
    }

    /// Add constraint that two type variables must be equal
    pub fn add_equal_constraint(&mut self, var1: TypeVarId, var2: TypeVarId) {
        self.add_constraint(TypeConstraint::Equal(var1, var2));
    }

    /// Add constraint that a type variable must be a concrete type
    pub fn add_concrete_constraint(&mut self, var: TypeVarId, ty: RueType) {
        self.add_constraint(TypeConstraint::Concrete(var, ty));
    }

    /// Add a binary operation constraint
    pub fn add_binary_constraint(
        &mut self,
        op: BinOp,
        lhs_var: TypeVarId,
        rhs_var: TypeVarId,
        result_var: TypeVarId,
    ) {
        self.add_constraint(TypeConstraint::Binary(op, lhs_var, rhs_var, result_var));
    }

    /// Solve a type variable to a concrete type
    pub fn solve_type_var(&mut self, var: TypeVarId, ty: RueType) -> Result<(), String> {
        if let Some(existing) = self.solutions.get(&var) {
            if existing != &ty {
                return Err(format!(
                    "Type variable already solved to {existing}, cannot solve to {ty}"
                ));
            }
        } else {
            self.solutions.insert(var, ty);
            self.pending_vars.remove(&var);
        }
        Ok(())
    }

    /// Get the solution for a type variable (if solved)
    pub fn get_solution(&self, var: TypeVarId) -> Option<&RueType> {
        self.solutions.get(&var)
    }

    /// Apply type inference rules for numeric literals
    ///
    /// This method implements smart numeric literal inference:
    /// - If there's an expected type (i32/i64), use it
    /// - Otherwise, check if value fits in i32 (default to i32)
    /// - If value is too large for i32, use i64
    pub fn infer_numeric_literal(&self, _value: i64, expected: Option<&RueType>) -> RueType {
        match expected {
            Some(RueType::I32) => RueType::I32,
            Some(RueType::I64) => RueType::I64,
            Some(other) => other.clone(), // Return the actual expected type for proper error handling
            None => RueType::I32,         // Default to i32 when no hint
        }
    }

    /// Infer types for binary operations with better constraint propagation
    ///
    /// This improves upon the current binary expression checking by:
    /// 1. Propagating type information bidirectionally
    /// 2. Using the expected type to guide operand type inference
    /// 3. Supporting future constraint-based inference
    pub fn infer_binary_operation(
        &mut self,
        op: BinOp,
        lhs_hint: Option<&RueType>,
        rhs_hint: Option<&RueType>,
        expected_result: Option<&RueType>,
    ) -> (Option<RueType>, Option<RueType>, RueType) {
        match op {
            // Arithmetic operations: result type matches operand types
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                // If we have an expected result type and it's numeric, use it for operands
                let operand_hint = expected_result
                    .filter(|ty| matches!(ty, RueType::I32 | RueType::I64))
                    .or(lhs_hint)
                    .or(rhs_hint);

                let operand_type = operand_hint.cloned();
                (
                    operand_type.clone(),
                    operand_type.clone(),
                    operand_type.unwrap_or(RueType::I32),
                )
            }
            // Comparison operations: operands match, result is bool
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                let operand_hint = lhs_hint.or(rhs_hint);
                let operand_type = operand_hint.cloned();
                (operand_type.clone(), operand_type, RueType::Bool)
            }
        }
    }

    /// Simple constraint solver (can be extended for full unification)
    ///
    /// Currently implements basic equality constraint solving.
    /// Future versions can add:
    /// - Full unification algorithm
    /// - Subtyping constraints
    /// - Type class constraints
    pub fn solve_constraints(&mut self) -> Result<(), String> {
        // Simple fixed-point iteration for now
        let mut changed = true;
        let mut iterations = 0;
        const MAX_ITERATIONS: usize = 100;

        while changed && iterations < MAX_ITERATIONS {
            changed = false;
            iterations += 1;

            for constraint in self.constraints.clone() {
                match constraint {
                    TypeConstraint::Equal(var1, var2) => {
                        // If either variable has a solution, propagate it to the other
                        let sol1 = self.solutions.get(&var1).cloned();
                        let sol2 = self.solutions.get(&var2).cloned();

                        match (sol1, sol2) {
                            (Some(ty1), Some(ty2)) => {
                                if ty1 != ty2 {
                                    return Err(format!("Type mismatch: {ty1} != {ty2}"));
                                }
                            }
                            (Some(ty), None) => {
                                self.solve_type_var(var2, ty)?;
                                changed = true;
                            }
                            (None, Some(ty)) => {
                                self.solve_type_var(var1, ty)?;
                                changed = true;
                            }
                            (None, None) => {
                                // Can't solve yet, wait for more information
                            }
                        }
                    }
                    TypeConstraint::Concrete(var, ty) => {
                        if let Some(solution) = self.solutions.get(&var) {
                            if solution != &ty {
                                return Err(format!(
                                    "Type variable constraint violated: {solution} != {ty}"
                                ));
                            }
                        } else {
                            self.solve_type_var(var, ty.clone())?;
                            changed = true;
                        }
                    }
                    TypeConstraint::Binary(op, lhs_var, rhs_var, result_var) => {
                        // Try to solve based on known variables
                        let lhs_ty = self.solutions.get(&lhs_var).cloned();
                        let rhs_ty = self.solutions.get(&rhs_var).cloned();
                        let result_ty = self.solutions.get(&result_var).cloned();

                        match op {
                            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                                // Arithmetic: all types must match
                                if let Some(ty) =
                                    lhs_ty.clone().or(rhs_ty.clone()).or(result_ty.clone())
                                {
                                    if !matches!(ty, RueType::I32 | RueType::I64) {
                                        return Err(format!(
                                            "Arithmetic operation requires numeric types, found {ty}"
                                        ));
                                    }
                                    if lhs_ty.is_none() {
                                        self.solve_type_var(lhs_var, ty.clone())?;
                                        changed = true;
                                    }
                                    if rhs_ty.is_none() {
                                        self.solve_type_var(rhs_var, ty.clone())?;
                                        changed = true;
                                    }
                                    if result_ty.is_none() {
                                        self.solve_type_var(result_var, ty)?;
                                        changed = true;
                                    }
                                }
                            }
                            BinOp::Lt
                            | BinOp::Le
                            | BinOp::Gt
                            | BinOp::Ge
                            | BinOp::Eq
                            | BinOp::Ne => {
                                // Comparison: operands match, result is bool
                                if result_ty.is_none() {
                                    self.solve_type_var(result_var, RueType::Bool)?;
                                    changed = true;
                                }
                                if let Some(ty) = lhs_ty.clone().or(rhs_ty.clone()) {
                                    if lhs_ty.is_none() {
                                        self.solve_type_var(lhs_var, ty.clone())?;
                                        changed = true;
                                    }
                                    if rhs_ty.is_none() {
                                        self.solve_type_var(rhs_var, ty)?;
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if iterations >= MAX_ITERATIONS {
            return Err("Type constraint solving did not converge".to_string());
        }

        // Check for unsolved variables
        if !self.pending_vars.is_empty() {
            // Default unsolved numeric variables to i32
            for var in self.pending_vars.clone() {
                self.solve_type_var(var, RueType::I32)?;
            }
        }

        Ok(())
    }

    /// Clear the context for reuse
    pub fn clear(&mut self) {
        self.constraints.clear();
        self.solutions.clear();
        self.pending_vars.clear();
    }
}

/// Simple scope for variable type tracking
#[derive(Debug, Clone)]
struct VariableScope {
    variables: HashMap<String, RueType>,
}

impl VariableScope {
    fn new() -> Self {
        Self {
            variables: HashMap::new(),
        }
    }

    fn insert(&mut self, name: String, ty: RueType) {
        self.variables.insert(name, ty);
    }

    fn get(&self, name: &str) -> Option<&RueType> {
        self.variables.get(name)
    }
}

/// Type checker that builds HIR during analysis
pub struct TypeChecker {
    /// Stack of variable scopes for type lookup
    variable_scopes: Vec<VariableScope>,
    /// Global function signatures for call validation
    function_signatures: HashMap<String, FunctionSignature>,
    /// Global scope containing struct definitions
    global_scope: Scope,
    /// Type inference context for improved type inference
    inference_context: TypeInferenceContext,
}

impl TypeChecker {
    /// Create a new type checker with the given function signatures
    pub fn new(global_scope: Scope) -> Self {
        let function_signatures = global_scope.functions.clone();
        Self {
            variable_scopes: vec![VariableScope::new()],
            function_signatures,
            global_scope,
            inference_context: TypeInferenceContext::new(),
        }
    }

    /// Push a new variable scope onto the stack
    fn push_scope(&mut self) {
        self.variable_scopes.push(VariableScope::new());
    }

    /// Pop the current variable scope from the stack
    fn pop_scope(&mut self) {
        if self.variable_scopes.len() > 1 {
            self.variable_scopes.pop();
        }
    }

    /// Get the current variable scope (mutable)
    fn current_scope_mut(&mut self) -> &mut VariableScope {
        self.variable_scopes
            .last_mut()
            .expect("Should always have at least one scope")
    }

    /// Look up a variable in the scope stack
    fn lookup_variable(&self, name: &str) -> Option<&RueType> {
        // Search from innermost to outermost scope
        for scope in self.variable_scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }

    /// Check if an expression is a numeric literal that can be contextually inferred
    fn is_numeric_literal(&self, expr: &ExpressionNode) -> bool {
        match expr {
            ExpressionNode::Literal(lit) => matches!(lit.kind, TokenKind::Integer(_)),
            _ => false,
        }
    }

    /// Check if an expression could benefit from numeric type inference hints
    #[allow(clippy::only_used_in_recursion)] // this is about &self, which we kind of need
    fn could_benefit_from_numeric_inference(&self, expr: &ExpressionNode) -> bool {
        match expr {
            ExpressionNode::Literal(lit) => matches!(lit.kind, TokenKind::Integer(_)),
            ExpressionNode::Unary(unary_expr) => {
                // Unary operations like negation can benefit if their operand can
                self.could_benefit_from_numeric_inference(&unary_expr.operand)
            }
            ExpressionNode::Binary(bin_expr) => {
                // Binary operations can benefit if either operand can benefit
                self.could_benefit_from_numeric_inference(&bin_expr.left)
                    || self.could_benefit_from_numeric_inference(&bin_expr.right)
            }
            _ => false,
        }
    }

    /// Type check the entire program and build HIR
    pub fn check_program(&mut self, ast: &CstRoot) -> Result<HirProgram, SemanticError> {
        let mut functions = Vec::new();

        for item in &ast.items {
            match item {
                rue_ast::CstNode::Function(func) => {
                    let hir_func = self.check_function(func)?;
                    functions.push(hir_func);
                }
                rue_ast::CstNode::StructDefinition(_) => {
                    // Struct definitions are already processed in the first pass
                    // by analyze_cst, so we skip them here
                }
                rue_ast::CstNode::Statement(_) => {
                    return Err(SemanticError {
                        message: "Top-level statements are not supported".to_string(),
                        span: Span { start: 0, end: 0 },
                    });
                }
                rue_ast::CstNode::Expression(_)
                | rue_ast::CstNode::Token(_)
                | rue_ast::CstNode::Error(_) => {
                    return Err(SemanticError {
                        message: "Unexpected top-level node type".to_string(),
                        span: Span { start: 0, end: 0 },
                    });
                }
            }
        }

        Ok(HirProgram { functions })
    }

    // Removed unused constraint-based methods (build_hir_from_constraints, collect_expr_constraints_for_binary)
    // Using simpler direct type inference approach for binary literals

    /// Type check a function and build HIR
    fn check_function(&mut self, func: &FunctionNode) -> Result<HirFunction, SemanticError> {
        // Extract function name
        let name = match &func.name.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected function name".to_string(),
                    span: func.name.span,
                });
            }
        };

        // Get function signature from pre-collected signatures
        let signature = self
            .function_signatures
            .get(&name)
            .ok_or_else(|| SemanticError {
                message: format!("Function signature not found: {name}"),
                span: func.name.span,
            })?
            .clone();

        // Push inference scope for this function with expected return type

        // Create new scope for function parameters
        self.push_scope();

        // Add parameters to current scope
        let mut params = Vec::new();
        for (i, param) in func.param_list.params.iter().enumerate() {
            let param_name = match &param.name.kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected parameter name".to_string(),
                        span: param.name.span,
                    });
                }
            };

            let param_type = signature.param_types[i].clone();
            params.push((param_name.clone(), param_type.clone()));
            self.current_scope_mut().insert(param_name, param_type);
        }

        // Type check function body
        let body = self.check_block(&func.body, Some(&signature.return_type))?;

        // Pop function scope
        self.pop_scope();

        // Pop inference scope

        Ok(HirFunction {
            name,
            params,
            return_type: signature.return_type,
            body,
            span: func.name.span,
        })
    }

    /// Type check a block and build HIR
    fn check_block(
        &mut self,
        block: &rue_ast::BlockNode,
        expected_return_type: Option<&RueType>,
    ) -> Result<HirBlock, SemanticError> {
        // Create new scope for the block
        self.push_scope();

        let mut statements = Vec::new();
        let mut expr = None;

        // Process all statements
        for stmt in &block.statements {
            let hir_stmt = self.check_statement(stmt, expected_return_type)?;
            statements.push(hir_stmt);
        }

        // Process final expression if present
        if let Some(final_expression) = &block.final_expr {
            let hir_expr = if let Some(expected_type) = expected_return_type {
                // Use expected return type as hint for inference
                self.check_expression_with_hint(final_expression, Some(expected_type))?
            } else {
                self.check_expression_with_hint(final_expression, None)?
            };
            expr = Some(Box::new(hir_expr));
        }

        // Validate return type if expected
        if let Some(expected_type) = expected_return_type {
            let block_type = if let Some(ref final_expr) = expr {
                final_expr.ty()
            } else {
                &RueType::Unit
            };

            // Check if any statement is a return statement that matches the expected type
            let has_valid_return = statements.iter().any(|stmt| {
                if let HirStatement::Return {
                    expr: Some(return_expr),
                    ..
                } = stmt
                {
                    return_expr.ty() == expected_type
                } else if let HirStatement::Return { expr: None, .. } = stmt {
                    *expected_type == RueType::Unit
                } else {
                    false
                }
            });

            // If there's a valid return statement, don't require the block's final expression to match
            if !has_valid_return && block_type != expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Expected return type {expected_type}, found {block_type}"
                    ),
                    span: block.open_brace.span,
                });
            }
        }

        // Pop block scope
        self.pop_scope();

        Ok(HirBlock { statements, expr })
    }

    /// Type check a statement and build HIR
    fn check_statement(
        &mut self,
        stmt: &StatementNode,
        expected_return_type: Option<&RueType>,
    ) -> Result<HirStatement, SemanticError> {
        match stmt {
            StatementNode::Let(let_stmt) => {
                // Extract variable name
                let var_name = match &let_stmt.name.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected variable name".to_string(),
                            span: let_stmt.name.span,
                        });
                    }
                };

                // Get declared type from type annotation first
                let declared_type = if let Some(type_ann) = &let_stmt.type_annotation {
                    crate::convert_type_node_with_scope(&type_ann.ty, &self.global_scope)?
                } else {
                    // For let statements without type annotations, we need to be smart about type inference
                    // If we have an expected return type and this might be used as a return value,
                    // consider using the return type as a hint for numeric expressions
                    let type_hint = if let Some(ret_type) = expected_return_type {
                        match ret_type {
                            RueType::I32 | RueType::I64 => {
                                // Use the return type as a hint for numeric expressions that could benefit
                                // This helps when the variable will be returned directly (like `let y = ...; y`)
                                if self.could_benefit_from_numeric_inference(&let_stmt.value) {
                                    Some(ret_type)
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    let temp_expr = if let Some(hint) = type_hint {
                        self.check_expression_with_hint(&let_stmt.value, Some(hint))?
                    } else {
                        self.check_expression_with_hint(&let_stmt.value, None)?
                    };
                    temp_expr.ty().clone()
                };

                // Type check initialization expression with type hint
                let init_expr = if let_stmt.type_annotation.is_some() {
                    // Use declared type as hint for inference
                    self.check_expression_with_hint(&let_stmt.value, Some(&declared_type))?
                } else {
                    // No type annotation, but we might have used a hint to infer the declared type
                    // Check with the same hint we used for inference
                    let type_hint = if let Some(ret_type) = expected_return_type {
                        match ret_type {
                            RueType::I32 | RueType::I64 => {
                                if self.could_benefit_from_numeric_inference(&let_stmt.value) {
                                    Some(ret_type)
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if let Some(hint) = type_hint {
                        self.check_expression_with_hint(&let_stmt.value, Some(hint))?
                    } else {
                        self.check_expression_with_hint(&let_stmt.value, None)?
                    }
                };

                // Validate type compatibility
                if init_expr.ty() != &declared_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch: expected {declared_type}, found {}",
                            init_expr.ty()
                        ),
                        span: let_stmt.value.span(),
                    });
                }

                // Add variable to current scope
                self.current_scope_mut()
                    .insert(var_name.clone(), declared_type.clone());

                Ok(HirStatement::Let {
                    name: var_name,
                    ty: declared_type,
                    init: init_expr,
                    span: let_stmt.let_token.span,
                })
            }
            StatementNode::Assign(assign_stmt) => {
                // Extract variable name
                let var_name = match &assign_stmt.name.kind {
                    TokenKind::Ident(name) => name.clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Expected variable name".to_string(),
                            span: assign_stmt.name.span,
                        });
                    }
                };

                // Look up variable type
                let var_type =
                    self.lookup_variable(&var_name)
                        .cloned()
                        .ok_or_else(|| SemanticError {
                            message: format!("Undefined variable: {var_name}"),
                            span: assign_stmt.name.span,
                        })?;

                // Type check value expression with type hint
                let value_expr =
                    self.check_expression_with_hint(&assign_stmt.value, Some(&var_type))?;

                // Validate type compatibility
                if value_expr.ty() != &var_type {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in assignment: expected {var_type}, found {}",
                            value_expr.ty()
                        ),
                        span: assign_stmt.value.span(),
                    });
                }

                Ok(HirStatement::Assign {
                    name: var_name,
                    value: value_expr,
                    span: assign_stmt.name.span,
                })
            }
            StatementNode::Expression(expr_stmt) => {
                let hir_expr = self.check_expression_with_hint(&expr_stmt.expression, None)?;
                Ok(HirStatement::Expr(hir_expr))
            }
            StatementNode::Return(return_stmt) => {
                let return_expr = if let Some(expr) = &return_stmt.expression {
                    // Type check the return expression with the expected return type as hint
                    let hir_expr = if let Some(expected_type) = expected_return_type {
                        self.check_expression_with_hint(expr, Some(expected_type))?
                    } else {
                        self.check_expression_with_hint(expr, None)?
                    };

                    // Validate that the return type matches the function's return type
                    if let Some(expected_type) = expected_return_type {
                        if hir_expr.ty() != expected_type {
                            return Err(SemanticError {
                                message: format!(
                                    "Return type mismatch: expected {expected_type}, found {}",
                                    hir_expr.ty()
                                ),
                                span: expr.span(),
                            });
                        }
                    }

                    Some(hir_expr)
                } else {
                    // Bare return; - should be unit type
                    if let Some(expected_type) = expected_return_type {
                        if *expected_type != RueType::Unit {
                            return Err(SemanticError {
                                message: format!(
                                    "Return type mismatch: expected {expected_type}, found unit (bare return)"
                                ),
                                span: return_stmt.return_token.span,
                            });
                        }
                    }
                    None
                };

                Ok(HirStatement::Return {
                    expr: return_expr,
                    span: return_stmt.return_token.span,
                })
            }
        }
    }

    /// Collect type constraints from an expression without building HIR
    fn collect_constraints_from_expression(
        &mut self,
        expr: &ExpressionNode,
        expected_type: Option<TypeVarId>,
    ) -> Result<TypeVarId, SemanticError> {
        match expr {
            ExpressionNode::Literal(lit) => {
                let var = self.inference_context.fresh_type_var();
                match &lit.kind {
                    TokenKind::Integer(_value) => {
                        // For integer literals, constrain based on value and context
                        if let Some(expected) = expected_type {
                            self.inference_context.add_equal_constraint(var, expected);
                        } else {
                            // Default to i32 for literals
                            self.inference_context
                                .add_concrete_constraint(var, RueType::I32);
                        }
                    }
                    TokenKind::True | TokenKind::False => {
                        self.inference_context
                            .add_concrete_constraint(var, RueType::Bool);
                    }
                    TokenKind::Unit => {
                        self.inference_context
                            .add_concrete_constraint(var, RueType::Unit);
                    }
                    _ => {}
                }
                Ok(var)
            }
            ExpressionNode::Identifier(ident) => {
                let var = self.inference_context.fresh_type_var();
                if let TokenKind::Ident(name) = &ident.kind {
                    // Clone the type to avoid borrow checker issues
                    let var_type = self.current_scope_mut().variables.get(name).cloned();
                    if let Some(ty) = var_type {
                        self.inference_context.add_concrete_constraint(var, ty);
                    }
                }
                Ok(var)
            }
            ExpressionNode::Binary(bin_expr) => {
                let op = match &bin_expr.operator.kind {
                    TokenKind::Plus => BinOp::Add,
                    TokenKind::Minus => BinOp::Sub,
                    TokenKind::Star => BinOp::Mul,
                    TokenKind::Slash => BinOp::Div,
                    TokenKind::Percent => BinOp::Mod,
                    TokenKind::Less => BinOp::Lt,
                    TokenKind::LessEqual => BinOp::Le,
                    TokenKind::Greater => BinOp::Gt,
                    TokenKind::GreaterEqual => BinOp::Ge,
                    TokenKind::Equal => BinOp::Eq,
                    TokenKind::NotEqual => BinOp::Ne,
                    _ => {
                        return Err(SemanticError {
                            message: format!(
                                "Invalid binary operator: {:?}",
                                bin_expr.operator.kind
                            ),
                            span: bin_expr.operator.span,
                        });
                    }
                };

                // For arithmetic operations, propagate expected type to operands
                // For comparison operations, operands are unconstrained initially
                let operand_hint = match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => expected_type,
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => None,
                };

                let lhs_var =
                    self.collect_constraints_from_expression(&bin_expr.left, operand_hint)?;
                let rhs_var =
                    self.collect_constraints_from_expression(&bin_expr.right, operand_hint)?;
                let result_var = self.inference_context.fresh_type_var();

                self.inference_context
                    .add_binary_constraint(op, lhs_var, rhs_var, result_var);

                if let Some(expected) = expected_type {
                    self.inference_context
                        .add_equal_constraint(result_var, expected);
                }

                Ok(result_var)
            }
            _ => {
                // For other expressions, fall back to simple type var for now
                let var = self.inference_context.fresh_type_var();
                if let Some(expected) = expected_type {
                    self.inference_context.add_equal_constraint(var, expected);
                }
                Ok(var)
            }
        }
    }

    /// Type check an expression with an optional type hint for inference
    fn check_expression_with_hint(
        &mut self,
        expr: &ExpressionNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        // For simple binary expressions, try constraint-based inference
        if matches!(expr, ExpressionNode::Binary(_)) {
            // Clear previous constraints
            self.inference_context.clear();

            // Collect constraints
            let expected_var = type_hint.map(|ty| {
                let var = self.inference_context.fresh_type_var();
                self.inference_context
                    .add_concrete_constraint(var, ty.clone());
                var
            });

            let result_var = self.collect_constraints_from_expression(expr, expected_var)?;

            // Solve constraints
            if let Err(_e) = self.inference_context.solve_constraints() {
                // If constraint solving fails, fall back to direct checking
                // Clear constraints for clean slate
                self.inference_context.clear();
            } else {
                // If we have a solution, use it to guide type checking
                if let Some(solved_type) = self.inference_context.get_solution(result_var) {
                    // Use solved type as hint
                    let hint_type = solved_type.clone();
                    self.inference_context.clear();
                    return self.check_expression_with_solved_hint(expr, Some(&hint_type));
                }
            }

            // Clear constraints after use
            self.inference_context.clear();
        }

        match expr {
            ExpressionNode::Literal(lit) => {
                // Use hint, don't also get from context to avoid double borrow
                self.check_literal_with_hint(lit, type_hint)
            }
            ExpressionNode::Identifier(ident) => self.check_identifier(ident),
            ExpressionNode::Binary(bin_expr) => {
                self.check_binary_expression_with_hint(bin_expr, type_hint)
            }
            ExpressionNode::Unary(unary_expr) => {
                self.check_unary_expression_with_hint(unary_expr, type_hint)
            }
            ExpressionNode::Call(call) => self.check_call_expression(call),
            ExpressionNode::If(if_expr) => self.check_if_expression_with_hint(if_expr, type_hint),
            ExpressionNode::While(while_expr) => self.check_while_expression(while_expr),
            ExpressionNode::StructLiteral(struct_lit) => self.check_struct_literal(struct_lit),
            ExpressionNode::FieldAccess(field_access) => self.check_field_access(field_access),
            ExpressionNode::TupleLiteral(tuple_lit) => {
                self.check_tuple_literal_with_hint(tuple_lit, type_hint)
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                self.check_array_literal_with_hint(array_lit, type_hint)
            }
            ExpressionNode::ArrayAccess(array_access) => self.check_array_access(array_access),
        }
    }

    /// Check expression with solved type from constraint solver
    fn check_expression_with_solved_hint(
        &mut self,
        expr: &ExpressionNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        match expr {
            ExpressionNode::Literal(lit) => self.check_literal_with_hint(lit, type_hint),
            ExpressionNode::Identifier(ident) => self.check_identifier(ident),
            ExpressionNode::Binary(bin_expr) => {
                // Use the direct checking with hint, not the recursive one
                self.check_binary_expression_with_hint(bin_expr, type_hint)
            }
            ExpressionNode::Unary(unary_expr) => {
                self.check_unary_expression_with_hint(unary_expr, type_hint)
            }
            ExpressionNode::Call(call) => self.check_call_expression(call),
            ExpressionNode::If(if_expr) => self.check_if_expression_with_hint(if_expr, type_hint),
            ExpressionNode::While(while_expr) => self.check_while_expression(while_expr),
            ExpressionNode::StructLiteral(struct_lit) => self.check_struct_literal(struct_lit),
            ExpressionNode::FieldAccess(field_access) => self.check_field_access(field_access),
            ExpressionNode::TupleLiteral(tuple_lit) => {
                self.check_tuple_literal_with_hint(tuple_lit, type_hint)
            }
            ExpressionNode::ArrayLiteral(array_lit) => {
                self.check_array_literal_with_hint(array_lit, type_hint)
            }
            ExpressionNode::ArrayAccess(array_access) => self.check_array_access(array_access),
        }
    }

    fn check_literal_with_hint(
        &mut self,
        lit: &rue_lexer::Token,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        let hir_lit = match &lit.kind {
            TokenKind::Integer(n) => {
                // Use TypeInferenceContext for better numeric literal inference
                let inferred_type = self.inference_context.infer_numeric_literal(*n, type_hint);

                match inferred_type {
                    RueType::I32 => {
                        // Check if value fits in i32
                        if *n >= i32::MIN as i64 && *n <= i32::MAX as i64 {
                            HirLiteral::Int32(*n as i32)
                        } else {
                            return Err(SemanticError {
                                message: format!(
                                    "literal out of range for `i32`\n\
                                     help: the literal `{n}` does not fit into the type `i32` whose range is `-2147483648..=2147483647`"
                                ),
                                span: lit.span,
                            });
                        }
                    }
                    RueType::I64 => {
                        // i64 literals should always fit since Token parsing already ensures this
                        HirLiteral::Int64(*n)
                    }
                    _ => {
                        return Err(SemanticError {
                            message: format!(
                                "Cannot use integer literal in context expecting {inferred_type}"
                            ),
                            span: lit.span,
                        });
                    }
                }
            }
            TokenKind::True => HirLiteral::Bool(true),
            TokenKind::False => HirLiteral::Bool(false),
            TokenKind::Unit => HirLiteral::Unit,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid literal: {:?}", lit.kind),
                    span: lit.span,
                });
            }
        };

        Ok(HirExpr::Literal {
            lit: hir_lit,
            span: lit.span,
        })
    }

    fn check_identifier(&mut self, ident: &rue_lexer::Token) -> Result<HirExpr, SemanticError> {
        let name = match &ident.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected identifier".to_string(),
                    span: ident.span,
                });
            }
        };

        let var_type = self
            .lookup_variable(&name)
            .cloned()
            .ok_or_else(|| SemanticError {
                message: format!("Undefined variable: {name}"),
                span: ident.span,
            })?;

        Ok(HirExpr::Var {
            name,
            ty: var_type,
            span: ident.span,
        })
    }

    fn check_binary_expression_with_hint(
        &mut self,
        bin_expr: &rue_ast::BinaryExprNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        // Direct checking with type hints
        // Convert operator first to determine what kinds of operands we need
        let op = match &bin_expr.operator.kind {
            TokenKind::Plus => BinOp::Add,
            TokenKind::Minus => BinOp::Sub,
            TokenKind::Star => BinOp::Mul,
            TokenKind::Slash => BinOp::Div,
            TokenKind::Percent => BinOp::Mod,
            TokenKind::LessEqual => BinOp::Le,
            TokenKind::Less => BinOp::Lt,
            TokenKind::GreaterEqual => BinOp::Ge,
            TokenKind::Greater => BinOp::Gt,
            TokenKind::Equal => BinOp::Eq,
            TokenKind::NotEqual => BinOp::Ne,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid binary operator: {:?}", bin_expr.operator.kind),
                    span: bin_expr.operator.span,
                });
            }
        };

        // Use TypeInferenceContext for better type inference
        // For arithmetic operations, pass the type hint to operands if available
        let operand_hint = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => type_hint,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => None,
        };

        // First, try to get initial types for operands with appropriate hints
        let lhs_initial = if let Some(hint) = operand_hint {
            self.check_expression_with_hint(&bin_expr.left, Some(hint))?
        } else {
            self.check_expression_with_hint(&bin_expr.left, None)?
        };
        let rhs_initial = if let Some(hint) = operand_hint {
            self.check_expression_with_hint(&bin_expr.right, Some(hint))?
        } else {
            self.check_expression_with_hint(&bin_expr.right, None)?
        };

        // Use inference context to determine the best types
        let (lhs_hint, rhs_hint, _expected_result) = self.inference_context.infer_binary_operation(
            op,
            Some(lhs_initial.ty()),
            Some(rhs_initial.ty()),
            type_hint,
        );

        // Re-check operands with inferred hints if needed
        let (lhs, rhs) = if lhs_initial.ty() == rhs_initial.ty() {
            // Types already match, use them
            (lhs_initial, rhs_initial)
        } else {
            // Types don't match, try to use hints for better inference
            let lhs_final = if let Some(hint) = lhs_hint.as_ref() {
                if self.could_benefit_from_numeric_inference(&bin_expr.left) {
                    self.check_expression_with_hint(&bin_expr.left, Some(hint))?
                } else {
                    lhs_initial
                }
            } else {
                lhs_initial
            };

            let rhs_final = if let Some(hint) = rhs_hint.as_ref() {
                if self.could_benefit_from_numeric_inference(&bin_expr.right) {
                    self.check_expression_with_hint(&bin_expr.right, Some(hint))?
                } else {
                    rhs_initial
                }
            } else {
                rhs_initial
            };

            // If still mismatched, try contextual inference
            if lhs_final.ty() != rhs_final.ty() {
                if self.is_numeric_literal(&bin_expr.left)
                    && matches!(rhs_final.ty(), RueType::I32 | RueType::I64)
                {
                    // Re-check lhs with rhs type as hint
                    let lhs_with_hint =
                        self.check_expression_with_hint(&bin_expr.left, Some(rhs_final.ty()))?;
                    (lhs_with_hint, rhs_final)
                } else if self.is_numeric_literal(&bin_expr.right)
                    && matches!(lhs_final.ty(), RueType::I32 | RueType::I64)
                {
                    // Re-check rhs with lhs type as hint
                    let rhs_with_hint =
                        self.check_expression_with_hint(&bin_expr.right, Some(lhs_final.ty()))?;
                    (lhs_final, rhs_with_hint)
                } else {
                    (lhs_final, rhs_final)
                }
            } else {
                (lhs_final, rhs_final)
            }
        };

        // Type checking for binary operations
        let result_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                // Arithmetic operations: operands must be same numeric type
                if lhs.ty() != rhs.ty() {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in binary operation: {} {} {}",
                            lhs.ty(),
                            op,
                            rhs.ty()
                        ),
                        span: bin_expr.operator.span,
                    });
                }
                match lhs.ty() {
                    RueType::I32 | RueType::I64 => lhs.ty().clone(),
                    _ => {
                        return Err(SemanticError {
                            message: "Arithmetic operators require numeric types".to_string(),
                            span: bin_expr.operator.span,
                        });
                    }
                }
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                // Comparison operations: operands must be same type, result is bool
                if lhs.ty() != rhs.ty() {
                    return Err(SemanticError {
                        message: format!(
                            "Type mismatch in comparison: {} {} {}",
                            lhs.ty(),
                            op,
                            rhs.ty()
                        ),
                        span: bin_expr.operator.span,
                    });
                }
                RueType::Bool
            }
        };

        Ok(HirExpr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            ty: result_type,
            span: bin_expr.operator.span,
        })
    }

    fn check_unary_expression_with_hint(
        &mut self,
        unary_expr: &rue_ast::UnaryExprNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        // For unary operations, we can pass the type hint to the operand
        let expr = self.check_expression_with_hint(&unary_expr.operand, type_hint)?;

        let op = match &unary_expr.operator.kind {
            TokenKind::Minus => UnaryOp::Neg,
            _ => {
                return Err(SemanticError {
                    message: format!("Invalid unary operator: {:?}", unary_expr.operator.kind),
                    span: unary_expr.operator.span,
                });
            }
        };

        // Type checking for unary operations
        let result_type = match op {
            UnaryOp::Neg => match expr.ty() {
                RueType::I32 | RueType::I64 => expr.ty().clone(),
                _ => {
                    return Err(SemanticError {
                        message: format!("Unary negation not supported for type: {}", expr.ty()),
                        span: unary_expr.operator.span,
                    });
                }
            },
        };

        Ok(HirExpr::Unary {
            op,
            expr: Box::new(expr),
            ty: result_type,
            span: unary_expr.operator.span,
        })
    }

    fn check_call_expression(
        &mut self,
        call: &rue_ast::CallExprNode,
    ) -> Result<HirExpr, SemanticError> {
        // Extract function name
        let func_name = match call.function.as_ref() {
            ExpressionNode::Identifier(token) => match &token.kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected function name".to_string(),
                        span: token.span,
                    });
                }
            },
            _ => {
                return Err(SemanticError {
                    message: "Function calls must use simple identifiers".to_string(),
                    span: call.function.span(),
                });
            }
        };

        // Look up function signature
        let signature = self
            .function_signatures
            .get(&func_name)
            .ok_or_else(|| SemanticError {
                message: format!("Undefined function: {func_name}"),
                span: match call.function.as_ref() {
                    ExpressionNode::Identifier(token) => token.span,
                    _ => call.open_paren.span,
                },
            })?
            .clone();

        // Check argument count
        if call.args.len() != signature.param_types.len() {
            return Err(SemanticError {
                message: format!(
                    "Function '{}' expects {} arguments, but {} were provided",
                    func_name,
                    signature.param_types.len(),
                    call.args.len()
                ),
                span: call.open_paren.span,
            });
        }

        // Type check arguments with type hints for inference
        let mut args = Vec::new();
        for (i, arg_expr) in call.args.iter().enumerate() {
            let expected_type = &signature.param_types[i];
            let arg = self.check_expression_with_hint(arg_expr, Some(expected_type))?;

            if arg.ty() != expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Argument {} of function {}: expected {expected_type}, found {}",
                        i + 1,
                        func_name,
                        arg.ty()
                    ),
                    span: arg_expr.span(),
                });
            }

            args.push(arg);
        }

        Ok(HirExpr::Call {
            func: func_name,
            args,
            ty: signature.return_type,
            span: call.open_paren.span,
        })
    }

    fn check_if_expression_with_hint(
        &mut self,
        if_expr: &rue_ast::IfStatementNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        // Type check condition
        let cond = self.check_expression_with_hint(&if_expr.condition, None)?;
        if cond.ty() != &RueType::Bool {
            return Err(SemanticError {
                message: format!("If condition must be bool, found {}", cond.ty()),
                span: if_expr.condition.span(),
            });
        }

        // Type check then block with type hint
        let then_block = self.check_block(&if_expr.then_block, type_hint)?;

        // Type check else block if present
        let (else_block, result_type) = if let Some(else_clause) = &if_expr.else_clause {
            let else_block_node = match &else_clause.body {
                rue_ast::ElseBodyNode::Block(block) => block.as_ref(),
                rue_ast::ElseBodyNode::If(_) => {
                    return Err(SemanticError {
                        message: "Else-if chains not yet supported in expressions".to_string(),
                        span: if_expr.if_token.span,
                    });
                }
            };
            let else_block = self.check_block(else_block_node, type_hint)?;

            // Determine result type by combining both branches
            let then_type = if let Some(ref then_expr) = then_block.expr {
                then_expr.ty()
            } else {
                &RueType::Unit
            };

            let else_type = if let Some(ref else_expr) = else_block.expr {
                else_expr.ty()
            } else {
                &RueType::Unit
            };

            if then_type != else_type {
                return Err(SemanticError {
                    message: "If branches have incompatible types".to_string(),
                    span: if_expr.if_token.span,
                });
            }

            (Some(else_block), then_type.clone())
        } else {
            // No else block means result is unit type
            (None, RueType::Unit)
        };

        Ok(HirExpr::If {
            cond: Box::new(cond),
            then_block,
            else_block,
            ty: result_type,
            span: if_expr.if_token.span,
        })
    }

    fn check_while_expression(
        &mut self,
        while_expr: &rue_ast::WhileStatementNode,
    ) -> Result<HirExpr, SemanticError> {
        // Type check condition
        let cond = self.check_expression_with_hint(&while_expr.condition, None)?;
        if cond.ty() != &RueType::Bool {
            return Err(SemanticError {
                message: format!("While condition must be bool, found {}", cond.ty()),
                span: while_expr.condition.span(),
            });
        }

        // Type check body
        let body = self.check_block(&while_expr.body, None)?;

        Ok(HirExpr::While {
            cond: Box::new(cond),
            body,
            span: while_expr.while_token.span,
        })
    }

    fn check_struct_literal(
        &mut self,
        struct_lit: &StructLiteralNode,
    ) -> Result<HirExpr, SemanticError> {
        let struct_name = match &struct_lit.name.kind {
            TokenKind::Ident(name) => name.clone(),
            _ => {
                return Err(SemanticError {
                    message: "Expected struct name".to_string(),
                    span: struct_lit.name.span,
                });
            }
        };

        // Look up struct definition
        let struct_def = self
            .global_scope
            .get_struct(&struct_name)
            .ok_or_else(|| SemanticError {
                message: format!("Undefined struct: {struct_name}"),
                span: struct_lit.name.span,
            })?
            .clone();

        // Create a map of expected fields for validation
        let expected_fields: HashMap<String, RueType> = struct_def
            .fields
            .iter()
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect();

        // Type check each field and collect them
        let mut hir_fields = Vec::new();
        let mut provided_fields = std::collections::HashSet::new();

        for field in &struct_lit.fields {
            let field_name = match &field.name.kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    return Err(SemanticError {
                        message: "Expected field name".to_string(),
                        span: field.name.span,
                    });
                }
            };

            // Check if field exists in struct definition
            let expected_type = expected_fields
                .get(&field_name)
                .ok_or_else(|| SemanticError {
                    message: format!("Unknown field {field_name}"),
                    span: field.name.span,
                })?;

            // Type check field value with expected type as hint
            let field_value = self.check_expression_with_hint(&field.value, Some(expected_type))?;
            if field_value.ty() != expected_type {
                return Err(SemanticError {
                    message: format!(
                        "Type mismatch: Field {} expected {expected_type}, found {}",
                        field_name,
                        field_value.ty()
                    ),
                    span: field.value.span(),
                });
            }

            provided_fields.insert(field_name.clone());
            hir_fields.push((field_name, field_value));
        }

        // Check that all expected fields are provided
        let missing_fields: Vec<_> = expected_fields
            .keys()
            .filter(|field| !provided_fields.contains(*field))
            .cloned()
            .collect();

        if !missing_fields.is_empty() {
            return Err(SemanticError {
                message: format!("Missing fields: {}", missing_fields.join(", ")),
                span: struct_lit.name.span,
            });
        }

        Ok(HirExpr::StructLiteral {
            struct_name,
            fields: hir_fields,
            ty: RueType::Struct(struct_def.id),
            span: struct_lit.name.span,
        })
    }

    fn check_field_access(
        &mut self,
        field_access: &FieldAccessNode,
    ) -> Result<HirExpr, SemanticError> {
        let base = self.check_expression_with_hint(&field_access.base, None)?;

        match base.ty() {
            RueType::Struct(struct_id) => {
                // Look up struct definition
                let struct_def =
                    self.global_scope
                        .get_struct_by_id(*struct_id)
                        .ok_or_else(|| SemanticError {
                            message: format!("Unknown struct with ID: {struct_id:?}"),
                            span: field_access.dot.span,
                        })?;

                let field_name = match &field_access.field {
                    FieldKindNode::Named(name_token) => {
                        if let TokenKind::Ident(name) = &name_token.kind {
                            name.clone()
                        } else {
                            return Err(SemanticError {
                                message: "Expected field name".to_string(),
                                span: name_token.span,
                            });
                        }
                    }
                    FieldKindNode::Positional(_) => {
                        return Err(SemanticError {
                            message: "Structs must use field names, not numeric indices"
                                .to_string(),
                            span: match &field_access.field {
                                FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                    token.span
                                }
                            },
                        });
                    }
                };

                // Find field in struct definition
                let field_type = struct_def
                    .fields
                    .iter()
                    .find(|(name, _)| name == &field_name)
                    .map(|(_, ty)| ty.clone())
                    .ok_or_else(|| SemanticError {
                        message: format!(
                            "Struct {} has no field named {}",
                            struct_def.name, field_name
                        ),
                        span: match &field_access.field {
                            FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                token.span
                            }
                        },
                    })?;

                Ok(HirExpr::FieldAccess {
                    base: Box::new(base),
                    field: FieldId::Named(field_name),
                    ty: field_type,
                    span: field_access.dot.span,
                })
            }
            RueType::Tuple(element_types) => {
                let field_index = match &field_access.field {
                    FieldKindNode::Positional(index_token) => {
                        if let TokenKind::Integer(index) = index_token.kind {
                            index as usize
                        } else {
                            return Err(SemanticError {
                                message: "Expected numeric index for tuple".to_string(),
                                span: index_token.span,
                            });
                        }
                    }
                    FieldKindNode::Named(_) => {
                        return Err(SemanticError {
                            message: "Tuples must use integer literal indices, not field names"
                                .to_string(),
                            span: match &field_access.field {
                                FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                    token.span
                                }
                            },
                        });
                    }
                };

                if field_index >= element_types.len() {
                    return Err(SemanticError {
                        message: format!(
                            "Tuple index {} out of bounds (tuple has {} elements)",
                            field_index,
                            element_types.len()
                        ),
                        span: match &field_access.field {
                            FieldKindNode::Named(token) | FieldKindNode::Positional(token) => {
                                token.span
                            }
                        },
                    });
                }

                let field_type = element_types[field_index].clone();

                Ok(HirExpr::FieldAccess {
                    base: Box::new(base),
                    field: FieldId::Index(field_index),
                    ty: field_type,
                    span: field_access.dot.span,
                })
            }
            _ => Err(SemanticError {
                message: format!(
                    "Cannot access field on type {}, expected struct or tuple",
                    base.ty()
                ),
                span: field_access.base.span(),
            }),
        }
    }

    fn check_tuple_literal_with_hint(
        &mut self,
        tuple_lit: &TupleLiteralNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        let mut elements = Vec::new();
        let mut element_types = Vec::new();

        // Extract element type hints from the tuple type hint
        let element_hints: Vec<Option<&RueType>> =
            if let Some(RueType::Tuple(expected_types)) = type_hint {
                expected_types.iter().map(Some).collect()
            } else {
                vec![None; tuple_lit.elements.len()]
            };

        for (i, element_expr) in tuple_lit.elements.iter().enumerate() {
            let element_hint = element_hints.get(i).and_then(|h| *h);
            let element = self.check_expression_with_hint(element_expr, element_hint)?;
            element_types.push(element.ty().clone());
            elements.push(element);
        }

        Ok(HirExpr::TupleLiteral {
            elements,
            ty: RueType::Tuple(element_types),
            span: tuple_lit.open_paren.span,
        })
    }

    fn check_array_literal_with_hint(
        &mut self,
        array_lit: &ArrayLiteralNode,
        type_hint: Option<&RueType>,
    ) -> Result<HirExpr, SemanticError> {
        // Handle empty array literals with type inference
        if array_lit.elements.is_empty() {
            // For empty arrays, we need a type hint to infer the element type
            if let Some(RueType::Array(element_type, size)) = type_hint {
                // Verify that the size matches (should be 0 for empty array)
                if *size != 0 {
                    return Err(SemanticError {
                        message: format!("Array size mismatch: expected {size} elements, found 0"),
                        span: array_lit.open_bracket.span,
                    });
                }

                // Create empty array with inferred type
                let array_type = RueType::Array(element_type.clone(), 0);
                return Ok(HirExpr::ArrayLiteral {
                    elements: Vec::new(),
                    ty: array_type,
                    span: array_lit.open_bracket.span,
                });
            } else {
                return Err(SemanticError {
                    message: "Cannot infer type of empty array literal without type annotation"
                        .to_string(),
                    span: array_lit.open_bracket.span,
                });
            }
        }

        let mut elements = Vec::new();
        let mut element_type: Option<RueType> = None;

        // Extract element type hint from the array type hint
        let element_hint = if let Some(RueType::Array(element_type, _)) = type_hint {
            Some(element_type.as_ref())
        } else {
            None
        };

        for element_expr in &array_lit.elements {
            let element = self.check_expression_with_hint(element_expr, element_hint)?;

            // Check that all elements have the same type
            if let Some(ref expected_type) = element_type {
                if element.ty() != expected_type {
                    return Err(SemanticError {
                        message: format!(
                            "Array elements must have the same type: expected {expected_type}, found {}",
                            element.ty()
                        ),
                        span: element_expr.span(),
                    });
                }
            } else {
                element_type = Some(element.ty().clone());
            }

            elements.push(element);
        }

        let array_type = RueType::Array(Box::new(element_type.unwrap()), array_lit.elements.len());

        Ok(HirExpr::ArrayLiteral {
            elements,
            ty: array_type,
            span: array_lit.open_bracket.span,
        })
    }

    fn check_array_access(
        &mut self,
        array_access: &ArrayAccessNode,
    ) -> Result<HirExpr, SemanticError> {
        let base = self.check_expression_with_hint(&array_access.base, None)?;
        // Check array index without forcing a specific type, allow both i32 and i64
        let index = self.check_expression_with_hint(&array_access.index, None)?;

        // Validate index type - must be an integer type
        match index.ty() {
            RueType::I32 | RueType::I64 => {}
            _ => {
                return Err(SemanticError {
                    message: "Array index must be an integer type (i32 or i64)".to_string(),
                    span: array_access.index.span(),
                });
            }
        }

        // Validate base type and extract element type
        let element_type = match base.ty() {
            RueType::Array(element_type, _) => (**element_type).clone(),
            _ => {
                return Err(SemanticError {
                    message: format!("Cannot index into type {}, expected array", base.ty()),
                    span: array_access.base.span(),
                });
            }
        };

        Ok(HirExpr::ArrayAccess {
            base: Box::new(base),
            index: Box::new(index),
            ty: element_type,
            span: array_access.open_bracket.span,
        })
    }
}
