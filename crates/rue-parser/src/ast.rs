//! Abstract Syntax Tree types for Rue.
//!
//! The AST represents the syntactic structure of the source code.
//! It closely mirrors the source syntax and preserves all information
//! needed for error reporting.

use rue_span::Span;

/// A complete source file (list of items).
#[derive(Debug, Clone)]
pub struct Ast {
    pub items: Vec<Item>,
}

/// A top-level item in a source file.
#[derive(Debug, Clone)]
pub enum Item {
    Function(Function),
    Struct(StructDecl),
}

/// A struct declaration.
#[derive(Debug, Clone)]
pub struct StructDecl {
    /// Struct name
    pub name: Ident,
    /// Struct fields
    pub fields: Vec<FieldDecl>,
    /// Span covering the entire struct declaration
    pub span: Span,
}

/// A field declaration in a struct.
#[derive(Debug, Clone)]
pub struct FieldDecl {
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: Ident,
    /// Span covering the entire field declaration
    pub span: Span,
}

/// A function definition.
#[derive(Debug, Clone)]
pub struct Function {
    /// Function name
    pub name: Ident,
    /// Function parameters
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<Ident>,
    /// Function body
    pub body: Expr,
    /// Span covering the entire function
    pub span: Span,
}

/// A function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    /// Parameter name
    pub name: Ident,
    /// Parameter type
    pub ty: Ident,
    /// Span covering the entire parameter
    pub span: Span,
}

/// An identifier.
#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// An expression.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Integer literal
    Int(IntLit),
    /// Boolean literal
    Bool(BoolLit),
    /// Identifier reference (variable)
    Ident(Ident),
    /// Binary operation (e.g., `a + b`)
    Binary(BinaryExpr),
    /// Unary operation (e.g., `-x`)
    Unary(UnaryExpr),
    /// Parenthesized expression (e.g., `(a + b)`)
    Paren(ParenExpr),
    /// Block with statements and final expression
    Block(BlockExpr),
    /// If expression (e.g., `if cond { a } else { b }`)
    If(IfExpr),
    /// Match expression (e.g., `match x { 1 => a, _ => b }`)
    Match(MatchExpr),
    /// While expression (e.g., `while cond { body }`)
    While(WhileExpr),
    /// Loop expression - infinite loop (e.g., `loop { body }`)
    Loop(LoopExpr),
    /// Function call (e.g., `foo(1, 2)`)
    Call(CallExpr),
    /// Break statement (exits the innermost loop)
    Break(BreakExpr),
    /// Continue statement (skips to the next iteration of the innermost loop)
    Continue(ContinueExpr),
    /// Return statement (returns a value from the current function)
    Return(ReturnExpr),
    /// Struct literal (e.g., `Point { x: 1, y: 2 }`)
    StructLit(StructLitExpr),
    /// Field access (e.g., `point.x`)
    Field(FieldExpr),
    /// Intrinsic call (e.g., `@dbg(42)`)
    IntrinsicCall(IntrinsicCallExpr),
}

/// An integer literal.
#[derive(Debug, Clone)]
pub struct IntLit {
    pub value: i64,
    pub span: Span,
}

/// A boolean literal.
#[derive(Debug, Clone)]
pub struct BoolLit {
    pub value: bool,
    pub span: Span,
}

/// A binary expression.
#[derive(Debug, Clone)]
pub struct BinaryExpr {
    pub left: Box<Expr>,
    pub op: BinaryOp,
    pub right: Box<Expr>,
    pub span: Span,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    // Arithmetic
    Add, // +
    Sub, // -
    Mul, // *
    Div, // /
    Mod, // %
    // Comparison
    Eq, // ==
    Ne, // !=
    Lt, // <
    Gt, // >
    Le, // <=
    Ge, // >=
    // Logical
    And, // &&
    Or,  // ||
}

/// A unary expression.
#[derive(Debug, Clone)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg, // -
    Not, // !
}

/// A parenthesized expression.
#[derive(Debug, Clone)]
pub struct ParenExpr {
    pub inner: Box<Expr>,
    pub span: Span,
}

/// A block expression containing statements and a final expression.
#[derive(Debug, Clone)]
pub struct BlockExpr {
    /// Statements in the block
    pub statements: Vec<Statement>,
    /// Final expression (the value of the block)
    pub expr: Box<Expr>,
    pub span: Span,
}

/// An if expression.
#[derive(Debug, Clone)]
pub struct IfExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Then branch
    pub then_block: BlockExpr,
    /// Optional else branch
    pub else_block: Option<BlockExpr>,
    pub span: Span,
}

/// A match expression.
#[derive(Debug, Clone)]
pub struct MatchExpr {
    /// The value being matched (scrutinee)
    pub scrutinee: Box<Expr>,
    /// Match arms
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

/// A single arm in a match expression.
#[derive(Debug, Clone)]
pub struct MatchArm {
    /// The pattern to match
    pub pattern: Pattern,
    /// The body expression
    pub body: Box<Expr>,
    pub span: Span,
}

/// A pattern in a match arm.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern
    Int(IntLit),
    /// Boolean literal pattern
    Bool(BoolLit),
}

impl Pattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(span) => *span,
            Pattern::Int(lit) => lit.span,
            Pattern::Bool(lit) => lit.span,
        }
    }
}

/// A function call expression.
#[derive(Debug, Clone)]
pub struct CallExpr {
    /// Function name
    pub name: Ident,
    /// Arguments
    pub args: Vec<Expr>,
    pub span: Span,
}

/// An intrinsic call expression (e.g., `@dbg(42)`).
#[derive(Debug, Clone)]
pub struct IntrinsicCallExpr {
    /// Intrinsic name (without the @)
    pub name: Ident,
    /// Arguments
    pub args: Vec<Expr>,
    pub span: Span,
}

/// A struct literal expression (e.g., `Point { x: 1, y: 2 }`).
#[derive(Debug, Clone)]
pub struct StructLitExpr {
    /// Struct type name
    pub name: Ident,
    /// Field initializers
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

/// A field initializer in a struct literal.
#[derive(Debug, Clone)]
pub struct FieldInit {
    /// Field name
    pub name: Ident,
    /// Field value
    pub value: Box<Expr>,
    pub span: Span,
}

/// A field access expression (e.g., `point.x`).
#[derive(Debug, Clone)]
pub struct FieldExpr {
    /// Base expression (the struct value)
    pub base: Box<Expr>,
    /// Field name
    pub field: Ident,
    pub span: Span,
}

/// A statement (does not produce a value).
#[derive(Debug, Clone)]
pub enum Statement {
    /// Let binding: `let x = expr;` or `let mut x = expr;`
    Let(LetStatement),
    /// Assignment: `x = expr;`
    Assign(AssignStatement),
    /// Expression statement: `expr;`
    Expr(Expr),
}

/// A let binding statement.
#[derive(Debug, Clone)]
pub struct LetStatement {
    /// Whether the binding is mutable
    pub is_mut: bool,
    /// Variable name
    pub name: Ident,
    /// Optional type annotation
    pub ty: Option<Ident>,
    /// Initializer expression
    pub init: Box<Expr>,
    pub span: Span,
}

/// An assignment statement.
#[derive(Debug, Clone)]
pub struct AssignStatement {
    /// Assignment target (variable or field)
    pub target: AssignTarget,
    /// Value expression
    pub value: Box<Expr>,
    pub span: Span,
}

/// An assignment target.
#[derive(Debug, Clone)]
pub enum AssignTarget {
    /// Variable assignment (e.g., `x = 5`)
    Var(Ident),
    /// Field assignment (e.g., `point.x = 5`)
    Field(FieldExpr),
}

/// A while loop expression.
#[derive(Debug, Clone)]
pub struct WhileExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// An infinite loop expression.
#[derive(Debug, Clone)]
pub struct LoopExpr {
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// A break expression (exits the innermost loop).
#[derive(Debug, Clone)]
pub struct BreakExpr {
    pub span: Span,
}

/// A continue expression (skips to the next iteration of the innermost loop).
#[derive(Debug, Clone)]
pub struct ContinueExpr {
    pub span: Span,
}

/// A return expression (returns a value from the current function).
#[derive(Debug, Clone)]
pub struct ReturnExpr {
    /// The value to return (None for `return;` in unit-returning functions)
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

impl Expr {
    /// Get the span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(lit) => lit.span,
            Expr::Bool(lit) => lit.span,
            Expr::Ident(ident) => ident.span,
            Expr::Binary(bin) => bin.span,
            Expr::Unary(un) => un.span,
            Expr::Paren(paren) => paren.span,
            Expr::Block(block) => block.span,
            Expr::If(if_expr) => if_expr.span,
            Expr::Match(match_expr) => match_expr.span,
            Expr::While(while_expr) => while_expr.span,
            Expr::Loop(loop_expr) => loop_expr.span,
            Expr::Call(call) => call.span,
            Expr::Break(break_expr) => break_expr.span,
            Expr::Continue(continue_expr) => continue_expr.span,
            Expr::Return(return_expr) => return_expr.span,
            Expr::StructLit(struct_lit) => struct_lit.span,
            Expr::Field(field_expr) => field_expr.span,
            Expr::IntrinsicCall(intrinsic) => intrinsic.span,
        }
    }
}
