//! High-level Intermediate Representation (HIR)
//!
//! HIR is the typed, desugared representation of a Rue program after semantic analysis.
//! It removes syntactic details like parentheses, semicolons, and preserves only the
//! essential semantic information needed for code generation.
//!
//! ## Architecture
//!
//! HIR sits between semantic analysis and code generation in the compiler pipeline:
//! ```text
//! AST + Types → HIR → Platform-Independent IR → Target Code
//! ```
//!
//! ## Key Properties
//!
//! - **Typed**: All expressions carry complete type information from semantic analysis
//! - **Desugared**: Syntactic details removed, only semantic meaning preserved  
//! - **Structured**: Clean hierarchy of programs, functions, blocks, statements, expressions
//! - **Location-aware**: Source spans preserved for error reporting and debugging
//! - **Validated**: Structural invariants maintained throughout construction
//!
//! ## Design Goals
//!
//! - Provide clean abstraction between language-specific and platform-specific concerns
//! - Enable optimization passes to operate on language constructs directly
//! - Simplify code generation by removing syntactic complexity
//! - Support multiple backends through stable interface
//! - Maintain debugging information for user experience
//!
//! ## Example Transformation
//!
//! Source: `if x <= 1 { x } else { x * 2 }`
//!
//! HIR:
//! ```text
//! HirExpr::If {
//!   cond: Binary { op: Le, lhs: Var("x"), rhs: Literal(Int32(1)), ty: Bool },
//!   then_block: Block { expr: Var("x") },
//!   else_block: Block { expr: Binary { op: Mul, lhs: Var("x"), rhs: Literal(Int32(2)) } },
//!   ty: I32
//! }
//! ```

use crate::types::RueType;
use rue_lexer::Span;
use std::fmt;

/// A complete HIR program consisting of functions.
///
/// Represents the top-level structure of a Rue program after semantic analysis.
/// All global state and declarations are contained within functions.
///
/// # Properties
/// - Contains all functions defined in the program
/// - Functions are ordered as they appear in source code
/// - No global variables or top-level statements (everything is in functions)
#[derive(Debug, Clone, PartialEq)]
pub struct HirProgram {
    /// All functions defined in the program, including `main`
    pub functions: Vec<HirFunction>,
}

/// A function in HIR form with typed parameters and body.
///
/// Represents a complete function definition after semantic analysis and type checking.
/// All parameter and return types are concrete and validated.
///
/// # Properties
/// - Function names are unique within a program
/// - All parameters have explicit types (no inference)
/// - Return type is always specified (may be unit for void functions)
/// - Body is a single block containing the function's implementation
/// - Source location preserved for error reporting
#[derive(Debug, Clone, PartialEq)]
pub struct HirFunction {
    /// Function identifier (e.g., "main", "factorial")
    pub name: String,
    /// Parameter list with names and concrete types
    pub params: Vec<(String, RueType)>,
    /// Return type (unit type for functions that don't return a value)
    pub return_type: RueType,
    /// Function body as a single block
    pub body: HirBlock,
    /// Source location of the function definition
    pub span: Span,
}

/// A block of statements with an optional final expression.
///
/// Represents a sequence of statements followed by an optional final expression
/// that determines the block's value. Corresponds to `{ ... }` syntax in source.
///
/// # Semantics
/// - Statements are executed in order for their side effects
/// - The final expression (if present) determines the block's type and value
/// - If no final expression, the block has unit type and value
/// - Creates a new variable scope (variables declared here are not visible outside)
///
/// # Examples
/// - `{ let x = 5; x + 1 }` → statements: [Let(x)], expr: Some(Binary(x + 1))
/// - `{ println_i32(42); }` → statements: [Expr(Call)], expr: None
#[derive(Debug, Clone, PartialEq)]
pub struct HirBlock {
    /// Statements executed for side effects (let bindings, assignments, expression statements)
    pub statements: Vec<HirStatement>,
    /// Final expression that determines block's value and type (None = unit type)
    pub expr: Option<Box<HirExpr>>,
}

/// Statements that don't produce values.
///
/// Represents operations that are executed for their side effects rather than
/// their return values. Statements modify program state but don't contribute
/// to the value of their containing block.
///
/// # Variants
/// - `Let`: Variable declaration and initialization
/// - `Assign`: Assignment to existing variable  
/// - `Expr`: Expression evaluated and discarded (e.g., function calls for side effects)
#[derive(Debug, Clone, PartialEq)]
pub enum HirStatement {
    /// Variable declaration: `let name: type = init;`
    ///
    /// Creates a new variable in the current scope with the given name and type,
    /// initialized to the value of the init expression.
    Let {
        /// Variable name being declared
        name: String,
        /// Explicit type annotation (required in Rue)
        ty: RueType,
        /// Initialization expression (must match declared type)
        init: HirExpr,
        /// Source location of the let statement
        span: Span,
    },
    /// Variable assignment: `name = value;`
    ///
    /// Updates an existing variable with a new value. The variable must already
    /// be declared in the current or outer scope.
    Assign {
        /// Name of variable being assigned to
        name: String,
        /// New value for the variable
        value: HirExpr,
        /// Source location of the assignment
        span: Span,
    },
    /// Expression statement: `expr;`
    ///
    /// Evaluates an expression for its side effects, discarding the return value.
    /// Common for function calls that don't return useful values.
    Expr(HirExpr),
}

/// Typed expressions in HIR.
///
/// Represents all expression forms in Rue after semantic analysis and type checking.
/// Every expression has a concrete type determined during semantic analysis.
///
/// # Type Safety
/// All expressions are guaranteed to be well-typed:
/// - Binary operations have compatible operand types
/// - Function calls have correct argument types and counts
/// - Control flow branches have compatible types
/// - Variable references are guaranteed to exist and have correct types
///
/// # Expression Categories
/// - **Values**: Literals, variables
/// - **Operations**: Binary operations, unary operations, function calls
/// - **Control Flow**: If expressions, while loops
#[derive(Debug, Clone, PartialEq)]
pub enum HirExpr {
    /// Literal value (42, true, (), etc.)
    ///
    /// Type is determined by the literal's form:
    /// - Integer literals default to i32, or inherit from context
    /// - Boolean literals are always bool type
    /// - Unit literal () is always unit type
    Literal {
        /// The literal value and its concrete type
        lit: HirLiteral,
        /// Source location of the literal
        span: Span,
    },

    /// Variable reference
    ///
    /// References a variable by name. Variable must be declared in current
    /// or enclosing scope. Type comes from the variable's declaration.
    Var {
        /// Name of the variable being referenced
        name: String,
        /// Type of the variable (from declaration)
        ty: RueType,
        /// Source location of the variable reference
        span: Span,
    },

    /// Binary operation (a + b, x <= y, etc.)
    ///
    /// Applies a binary operator to two sub-expressions. Operands and result
    /// types are validated during semantic analysis.
    Binary {
        /// Binary operator being applied
        op: BinOp,
        /// Left operand expression
        lhs: Box<HirExpr>,
        /// Right operand expression
        rhs: Box<HirExpr>,
        /// Result type of the operation
        ty: RueType,
        /// Source location of the operation
        span: Span,
    },
    Unary {
        op: UnaryOp,
        expr: Box<HirExpr>,
        ty: RueType,
        span: Span,
    },
    /// Function call expression
    ///
    /// Calls a named function with the given arguments. Function name,
    /// argument count, and types are validated during semantic analysis.
    Call {
        /// Name of function being called
        func: String,
        /// Argument expressions (evaluated left to right)
        args: Vec<HirExpr>,
        /// Return type of the function
        ty: RueType,
        /// Source location of the call
        span: Span,
    },

    /// If expression with optional else branch
    ///
    /// Evaluates condition and executes appropriate branch. If no else branch
    /// is provided, the expression has unit type. Both branches must have
    /// compatible types when else is present.
    If {
        /// Condition expression (must be bool type)
        cond: Box<HirExpr>,
        /// Block executed when condition is true
        then_block: HirBlock,
        /// Optional block executed when condition is false
        else_block: Option<HirBlock>,
        /// Result type (unit if no else, common type of both branches if else present)
        ty: RueType,
        /// Source location of the if expression
        span: Span,
    },

    /// While loop expression
    ///
    /// Repeatedly evaluates the body while the condition is true.
    /// Always has unit type since loops don't produce values.
    While {
        /// Condition expression (must be bool type)
        cond: Box<HirExpr>,
        /// Loop body block
        body: HirBlock,
        /// Source location of the while loop
        span: Span,
    },
}

/// Literal values with concrete types.
///
/// Represents compile-time constant values in the program. Each variant
/// corresponds to a specific Rue type and carries the literal's value.
///
/// # Type Mapping
/// - `Int32(n)` → `RueType::I32`
/// - `Int64(n)` → `RueType::I64`  
/// - `Bool(b)` → `RueType::Bool`
/// - `Unit` → `RueType::Unit`
#[derive(Debug, Clone, PartialEq)]
pub enum HirLiteral {
    /// 32-bit signed integer literal
    Int32(i32),
    /// 64-bit signed integer literal  
    Int64(i64),
    /// Boolean literal (true or false)
    Bool(bool),
    /// Unit literal ()
    Unit,
}

/// Binary operators supported in Rue.
///
/// All binary operators are validated during semantic analysis to ensure
/// operand types are compatible and result types are correct.
///
/// # Operator Categories
/// - **Arithmetic**: Work on numeric types (i32, i64), produce same type
/// - **Comparison**: Work on compatible types, produce bool type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // Arithmetic operators (numeric → numeric)
    /// Addition (+)
    Add,
    /// Subtraction (-)
    Sub,
    /// Multiplication (*)
    Mul,
    /// Division (/)
    Div,
    /// Modulo (%)
    Mod,

    // Comparison operators (compatible types → bool)
    /// Less than (<)
    Lt,
    /// Less than or equal (<=)
    Le,
    /// Greater than (>)
    Gt,
    /// Greater than or equal (>=)
    Ge,
    /// Equality (==)
    Eq,
    /// Inequality (!=)
    Ne,
}

/// Unary operators supported in Rue.
///
/// Currently only numeric negation is supported. Future extensions
/// might include logical negation (!), bitwise operations, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Numeric negation (-expr)
    ///
    /// Applies to numeric types (i32, i64) and produces the same type.
    /// Semantically equivalent to `0 - expr`.
    Neg,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinOp::Add => write!(f, "+"),
            BinOp::Sub => write!(f, "-"),
            BinOp::Mul => write!(f, "*"),
            BinOp::Div => write!(f, "/"),
            BinOp::Mod => write!(f, "%"),
            BinOp::Lt => write!(f, "<"),
            BinOp::Le => write!(f, "<="),
            BinOp::Gt => write!(f, ">"),
            BinOp::Ge => write!(f, ">="),
            BinOp::Eq => write!(f, "=="),
            BinOp::Ne => write!(f, "!="),
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnaryOp::Neg => write!(f, "-"),
        }
    }
}

// Helper methods for working with HIR expressions
impl HirExpr {
    /// Get the type of this expression.
    ///
    /// Returns a reference to the type information for this expression.
    /// All HIR expressions are guaranteed to have concrete, validated types.
    ///
    /// # Examples
    /// - `HirExpr::Literal { lit: Int32(42), .. }.ty()` → `&RueType::I32`
    /// - `HirExpr::Binary { ty: Bool, .. }.ty()` → `&RueType::Bool`
    /// - `HirExpr::While { .. }.ty()` → `&RueType::Unit`
    pub fn ty(&self) -> &RueType {
        match self {
            HirExpr::Literal { lit, .. } => match lit {
                HirLiteral::Int32(_) => &RueType::I32,
                HirLiteral::Int64(_) => &RueType::I64,
                HirLiteral::Bool(_) => &RueType::Bool,
                HirLiteral::Unit => &RueType::Unit,
            },
            HirExpr::Var { ty, .. } => ty,
            HirExpr::Binary { ty, .. } => ty,
            HirExpr::Unary { ty, .. } => ty,
            HirExpr::Call { ty, .. } => ty,
            HirExpr::If { ty, .. } => ty,
            HirExpr::While { .. } => &RueType::Unit, // While always returns unit
        }
    }
}

// Pretty printing for debugging
impl fmt::Display for HirProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for func in &self.functions {
            writeln!(f, "{func}")?;
        }
        Ok(())
    }
}

impl fmt::Display for HirFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn {}(", self.name)?;
        for (i, (name, ty)) in self.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{name}: {ty}")?;
        }
        write!(f, ") -> {} ", self.return_type)?;
        write!(f, "{}", self.body)
    }
}

impl fmt::Display for HirBlock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{{")?;
        for stmt in &self.statements {
            writeln!(f, "  {stmt}")?;
        }
        if let Some(expr) = &self.expr {
            writeln!(f, "  {expr}")?;
        }
        write!(f, "}}")
    }
}

impl fmt::Display for HirStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HirStatement::Let { name, ty, init, .. } => {
                write!(f, "let {name}: {ty} = {init};")
            }
            HirStatement::Assign { name, value, .. } => {
                write!(f, "{name} = {value};")
            }
            HirStatement::Expr(expr) => {
                write!(f, "{expr};")
            }
        }
    }
}

impl fmt::Display for HirExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HirExpr::Literal { lit, .. } => write!(f, "{lit:?}"),
            HirExpr::Var { name, .. } => write!(f, "{name}"),
            HirExpr::Binary { op, lhs, rhs, .. } => {
                write!(f, "({lhs} {op} {rhs})")
            }
            HirExpr::Unary { op, expr, .. } => {
                write!(f, "({op}{expr})")
            }
            HirExpr::Call { func, args, .. } => {
                write!(f, "{func}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            HirExpr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                write!(f, "if {cond} {then_block} ")?;
                if let Some(else_block) = else_block {
                    write!(f, "else {else_block} ")?;
                }
                Ok(())
            }
            HirExpr::While { cond, body, .. } => {
                write!(f, "while {cond} {body}")
            }
        }
    }
}

#[cfg(test)]
mod tests;
