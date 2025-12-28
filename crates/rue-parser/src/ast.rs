//! Abstract Syntax Tree types for Rue.
//!
//! The AST represents the syntactic structure of the source code.
//! It closely mirrors the source syntax and preserves all information
//! needed for error reporting.

use std::fmt;

use rue_intern::Symbol;
use rue_span::Span;

/// A complete source file (list of items).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ast {
    pub items: Vec<Item>,
}

/// A directive that modifies compiler behavior for the following item or statement.
///
/// Directives use the `@name(args)` syntax and appear before items or statements.
/// For example: `@allow(unused_variable)`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// The directive name (without the @)
    pub name: Ident,
    /// Arguments to the directive
    pub args: Vec<DirectiveArg>,
    /// Span covering the entire directive
    pub span: Span,
}

/// An argument to a directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveArg {
    /// An identifier argument (e.g., `unused_variable` in `@allow(unused_variable)`)
    Ident(Ident),
}

/// A top-level item in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Function(Function),
    Struct(StructDecl),
    Enum(EnumDecl),
    Impl(ImplBlock),
    DropFn(DropFn),
}

/// A struct declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    /// Directives applied to this struct (e.g., @copy)
    pub directives: Vec<Directive>,
    /// Struct name
    pub name: Ident,
    /// Struct fields
    pub fields: Vec<FieldDecl>,
    /// Span covering the entire struct declaration
    pub span: Span,
}

/// A field declaration in a struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the entire field declaration
    pub span: Span,
}

/// An enum declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    /// Enum name
    pub name: Ident,
    /// Enum variants
    pub variants: Vec<EnumVariant>,
    /// Span covering the entire enum declaration
    pub span: Span,
}

/// A variant in an enum declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    /// Variant name
    pub name: Ident,
    /// Span covering the variant
    pub span: Span,
}

/// An impl block containing methods for a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplBlock {
    /// The type this impl block is for
    pub type_name: Ident,
    /// Methods in this impl block
    pub methods: Vec<Method>,
    /// Span covering the entire impl block
    pub span: Span,
}

/// A user-defined destructor declaration.
///
/// Syntax: `drop fn TypeName(self) { body }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropFn {
    /// The struct type this destructor is for
    pub type_name: Ident,
    /// The self parameter
    pub self_param: SelfParam,
    /// Destructor body
    pub body: Expr,
    /// Span covering the entire drop fn
    pub span: Span,
}

/// A method definition in an impl block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    /// Directives applied to this method
    pub directives: Vec<Directive>,
    /// Method name
    pub name: Ident,
    /// Whether this method takes self (None = associated function, Some = method with receiver)
    pub receiver: Option<SelfParam>,
    /// Method parameters (excluding self)
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// Method body
    pub body: Expr,
    /// Span covering the entire method
    pub span: Span,
}

/// A self parameter in a method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfParam {
    /// Span covering the `self` keyword
    pub span: Span,
}

/// A function definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Directives applied to this function
    pub directives: Vec<Directive>,
    /// Function name
    pub name: Ident,
    /// Function parameters
    pub params: Vec<Param>,
    /// Return type (None means implicit unit `()`)
    pub return_type: Option<TypeExpr>,
    /// Function body
    pub body: Expr,
    /// Span covering the entire function
    pub span: Span,
}

/// Parameter passing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    /// Normal pass-by-value parameter
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
}

impl Default for ParamMode {
    fn default() -> Self {
        ParamMode::Normal
    }
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Parameter passing mode (normal or inout)
    pub mode: ParamMode,
    /// Parameter name
    pub name: Ident,
    /// Parameter type
    pub ty: TypeExpr,
    /// Span covering the entire parameter
    pub span: Span,
}

/// An identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ident {
    pub name: Symbol,
    pub span: Span,
}

/// A type expression in the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    /// A simple named type (e.g., i32, bool, MyStruct)
    Named(Ident),
    /// Unit type: ()
    Unit(Span),
    /// Never type: !
    Never(Span),
    /// Array type: [T; N] where T is the element type and N is the length
    Array {
        element: Box<TypeExpr>,
        length: u64,
        span: Span,
    },
}

impl TypeExpr {
    /// Get the span of this type expression.
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named(ident) => ident.span,
            TypeExpr::Unit(span) => *span,
            TypeExpr::Never(span) => *span,
            TypeExpr::Array { span, .. } => *span,
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeExpr::Named(ident) => write!(f, "sym:{}", ident.name.as_u32()),
            TypeExpr::Unit(_) => write!(f, "()"),
            TypeExpr::Never(_) => write!(f, "!"),
            TypeExpr::Array {
                element, length, ..
            } => write!(f, "[{}; {}]", element, length),
        }
    }
}

/// A unit literal expression - represents `()` or implicit unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitLit {
    pub span: Span,
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Integer literal
    Int(IntLit),
    /// String literal
    String(StringLit),
    /// Boolean literal
    Bool(BoolLit),
    /// Unit literal (explicit `()` or implicit unit for blocks without final expression)
    Unit(UnitLit),
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
    /// Method call (e.g., `point.distance()`)
    MethodCall(MethodCallExpr),
    /// Intrinsic call (e.g., `@dbg(42)`)
    IntrinsicCall(IntrinsicCallExpr),
    /// Array literal (e.g., `[1, 2, 3]`)
    ArrayLit(ArrayLitExpr),
    /// Array indexing (e.g., `arr[0]`)
    Index(IndexExpr),
    /// Path expression (e.g., `Color::Red`)
    Path(PathExpr),
    /// Associated function call (e.g., `Point::origin()`)
    AssocFnCall(AssocFnCallExpr),
    /// Self expression (e.g., `self` in method bodies)
    SelfExpr(SelfExpr),
}

/// An integer literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntLit {
    pub value: u64,
    pub span: Span,
}

/// A string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringLit {
    pub value: Symbol,
    pub span: Span,
}

/// A boolean literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoolLit {
    pub value: bool,
    pub span: Span,
}

/// A binary expression.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    // Bitwise
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

/// A unary expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnaryExpr {
    pub op: UnaryOp,
    pub operand: Box<Expr>,
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,    // -
    Not,    // !
    BitNot, // ~
}

/// A parenthesized expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParenExpr {
    pub inner: Box<Expr>,
    pub span: Span,
}

/// A block expression containing statements and a final expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockExpr {
    /// Statements in the block
    pub statements: Vec<Statement>,
    /// Final expression (the value of the block)
    pub expr: Box<Expr>,
    pub span: Span,
}

/// An if expression.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchExpr {
    /// The value being matched (scrutinee)
    pub scrutinee: Box<Expr>,
    /// Match arms
    pub arms: Vec<MatchArm>,
    pub span: Span,
}

/// A single arm in a match expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    /// The pattern to match
    pub pattern: Pattern,
    /// The body expression
    pub body: Box<Expr>,
    pub span: Span,
}

/// A pattern in a match arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern (positive or zero)
    Int(IntLit),
    /// Negative integer literal pattern (e.g., `-1`, `-42`)
    NegInt(NegIntLit),
    /// Boolean literal pattern
    Bool(BoolLit),
    /// Path pattern (e.g., `Color::Red` for enum variant)
    Path(PathPattern),
}

/// A negative integer literal pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegIntLit {
    /// The absolute value of the negative integer
    pub value: u64,
    /// Span covering the entire pattern (minus sign and literal)
    pub span: Span,
}

/// A path pattern (e.g., `Color::Red` for enum variant matching).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPattern {
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    pub span: Span,
}

impl Pattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard(span) => *span,
            Pattern::Int(lit) => lit.span,
            Pattern::NegInt(lit) => lit.span,
            Pattern::Bool(lit) => lit.span,
            Pattern::Path(path) => path.span,
        }
    }
}

/// Argument passing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

/// An argument in a function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArg {
    /// The passing mode for this argument
    pub mode: ArgMode,
    /// The argument expression
    pub expr: Expr,
    /// Span covering the entire argument (including inout/borrow keyword if present)
    pub span: Span,
}

impl CallArg {
    /// Returns true if this argument is passed as inout.
    /// This is a convenience method for backwards compatibility.
    pub fn is_inout(&self) -> bool {
        self.mode == ArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow.
    pub fn is_borrow(&self) -> bool {
        self.mode == ArgMode::Borrow
    }
}

/// A function call expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallExpr {
    /// Function name
    pub name: Ident,
    /// Arguments
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// An argument to an intrinsic call (can be an expression or a type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntrinsicArg {
    /// An expression argument (e.g., `@dbg(42)`)
    Expr(Expr),
    /// A type argument (e.g., `@size_of(i32)`)
    Type(TypeExpr),
}

/// An intrinsic call expression (e.g., `@dbg(42)` or `@size_of(i32)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrinsicCallExpr {
    /// Intrinsic name (without the @)
    pub name: Ident,
    /// Arguments (can be expressions or types)
    pub args: Vec<IntrinsicArg>,
    pub span: Span,
}

/// A struct literal expression (e.g., `Point { x: 1, y: 2 }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLitExpr {
    /// Struct type name
    pub name: Ident,
    /// Field initializers
    pub fields: Vec<FieldInit>,
    pub span: Span,
}

/// A field initializer in a struct literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInit {
    /// Field name
    pub name: Ident,
    /// Field value
    pub value: Box<Expr>,
    pub span: Span,
}

/// A field access expression (e.g., `point.x`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldExpr {
    /// Base expression (the struct value)
    pub base: Box<Expr>,
    /// Field name
    pub field: Ident,
    pub span: Span,
}

/// A method call expression (e.g., `point.distance()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodCallExpr {
    /// Base expression (the receiver)
    pub receiver: Box<Expr>,
    /// Method name
    pub method: Ident,
    /// Arguments (excluding self)
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// An array literal expression (e.g., `[1, 2, 3]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayLitExpr {
    /// Array elements
    pub elements: Vec<Expr>,
    pub span: Span,
}

/// An array index expression (e.g., `arr[0]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexExpr {
    /// The array being indexed
    pub base: Box<Expr>,
    /// The index expression
    pub index: Box<Expr>,
    pub span: Span,
}

/// A path expression (e.g., `Color::Red` for enum variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathExpr {
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    pub span: Span,
}

/// An associated function call expression (e.g., `Point::origin()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocFnCallExpr {
    /// The type name (e.g., `Point`)
    pub type_name: Ident,
    /// The function name (e.g., `origin`)
    pub function: Ident,
    /// Arguments
    pub args: Vec<CallArg>,
    pub span: Span,
}

/// A statement (does not produce a value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// Let binding: `let x = expr;` or `let mut x = expr;`
    Let(LetStatement),
    /// Assignment: `x = expr;`
    Assign(AssignStatement),
    /// Expression statement: `expr;`
    Expr(Expr),
}

/// A pattern in a let binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LetPattern {
    /// Named binding (e.g., `x`, `_unused`)
    Ident(Ident),
    /// Wildcard pattern `_` - discards the value without creating a binding
    Wildcard(Span),
}

impl LetPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            LetPattern::Ident(ident) => ident.span,
            LetPattern::Wildcard(span) => *span,
        }
    }
}

/// A let binding statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetStatement {
    /// Directives applied to this let binding
    pub directives: Vec<Directive>,
    /// Whether the binding is mutable
    pub is_mut: bool,
    /// The binding pattern (identifier or wildcard)
    pub pattern: LetPattern,
    /// Optional type annotation
    pub ty: Option<TypeExpr>,
    /// Initializer expression
    pub init: Box<Expr>,
    pub span: Span,
}

/// An assignment statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignStatement {
    /// Assignment target (variable or field)
    pub target: AssignTarget,
    /// Value expression
    pub value: Box<Expr>,
    pub span: Span,
}

/// An assignment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignTarget {
    /// Variable assignment (e.g., `x = 5`)
    Var(Ident),
    /// Field assignment (e.g., `point.x = 5`)
    Field(FieldExpr),
    /// Index assignment (e.g., `arr[0] = 5`)
    Index(IndexExpr),
}

/// A while loop expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhileExpr {
    /// Condition (must be bool)
    pub cond: Box<Expr>,
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// An infinite loop expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopExpr {
    /// Loop body
    pub body: BlockExpr,
    pub span: Span,
}

/// A break expression (exits the innermost loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakExpr {
    pub span: Span,
}

/// A continue expression (skips to the next iteration of the innermost loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueExpr {
    pub span: Span,
}

/// A return expression (returns a value from the current function).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnExpr {
    /// The value to return (None for `return;` in unit-returning functions)
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

/// A self expression (the `self` keyword in method bodies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfExpr {
    pub span: Span,
}

impl Expr {
    /// Get the span of this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(lit) => lit.span,
            Expr::String(lit) => lit.span,
            Expr::Bool(lit) => lit.span,
            Expr::Unit(lit) => lit.span,
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
            Expr::MethodCall(method_call) => method_call.span,
            Expr::IntrinsicCall(intrinsic) => intrinsic.span,
            Expr::ArrayLit(array_lit) => array_lit.span,
            Expr::Index(index_expr) => index_expr.span,
            Expr::Path(path_expr) => path_expr.span,
            Expr::AssocFnCall(assoc_fn_call) => assoc_fn_call.span,
            Expr::SelfExpr(self_expr) => self_expr.span,
        }
    }
}

// Display implementations for AST pretty-printing

impl fmt::Display for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            match item {
                Item::Function(func) => fmt_function(f, func, 0)?,
                Item::Struct(s) => fmt_struct(f, s, 0)?,
                Item::Enum(e) => fmt_enum(f, e, 0)?,
                Item::Impl(impl_block) => fmt_impl_block(f, impl_block, 0)?,
                Item::DropFn(drop_fn) => fmt_drop_fn(f, drop_fn, 0)?,
            }
        }
        Ok(())
    }
}

fn indent(f: &mut fmt::Formatter<'_>, level: usize) -> fmt::Result {
    for _ in 0..level {
        write!(f, "  ")?;
    }
    Ok(())
}

fn fmt_struct(f: &mut fmt::Formatter<'_>, s: &StructDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &s.directives {
        write!(f, "@sym:{} ", directive.name.name.as_u32())?;
    }
    writeln!(f, "Struct sym:{}", s.name.name.as_u32())?;
    for field in &s.fields {
        indent(f, level + 1)?;
        writeln!(f, "Field sym:{} : {}", field.name.name.as_u32(), field.ty)?;
    }
    Ok(())
}

fn fmt_enum(f: &mut fmt::Formatter<'_>, e: &EnumDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "Enum sym:{}", e.name.name.as_u32())?;
    for variant in &e.variants {
        indent(f, level + 1)?;
        writeln!(f, "Variant sym:{}", variant.name.name.as_u32())?;
    }
    Ok(())
}

fn fmt_impl_block(f: &mut fmt::Formatter<'_>, impl_block: &ImplBlock, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "Impl sym:{}", impl_block.type_name.name.as_u32())?;
    for method in &impl_block.methods {
        fmt_method(f, method, level + 1)?;
    }
    Ok(())
}

fn fmt_drop_fn(f: &mut fmt::Formatter<'_>, drop_fn: &DropFn, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "DropFn sym:{}(self)", drop_fn.type_name.name.as_u32())?;
    fmt_expr(f, &drop_fn.body, level + 1)?;
    Ok(())
}

fn fmt_method(f: &mut fmt::Formatter<'_>, method: &Method, level: usize) -> fmt::Result {
    indent(f, level)?;
    write!(f, "Method sym:{}", method.name.name.as_u32())?;
    write!(f, "(")?;
    if method.receiver.is_some() {
        write!(f, "self")?;
        if !method.params.is_empty() {
            write!(f, ", ")?;
        }
    }
    for (i, param) in method.params.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        fmt_param(f, param)?;
    }
    write!(f, ")")?;
    if let Some(ref ret) = method.return_type {
        write!(f, " -> {}", ret)?;
    }
    writeln!(f)?;
    fmt_expr(f, &method.body, level + 1)?;
    Ok(())
}

fn fmt_param(f: &mut fmt::Formatter<'_>, param: &Param) -> fmt::Result {
    match param.mode {
        ParamMode::Inout => write!(f, "inout ")?,
        ParamMode::Borrow => write!(f, "borrow ")?,
        ParamMode::Normal => {}
    }
    write!(f, "sym:{}: {}", param.name.name.as_u32(), param.ty)
}

fn fmt_call_arg(f: &mut fmt::Formatter<'_>, arg: &CallArg, level: usize) -> fmt::Result {
    match arg.mode {
        ArgMode::Inout => {
            indent(f, level)?;
            writeln!(f, "inout:")?;
            fmt_expr(f, &arg.expr, level + 1)
        }
        ArgMode::Borrow => {
            indent(f, level)?;
            writeln!(f, "borrow:")?;
            fmt_expr(f, &arg.expr, level + 1)
        }
        ArgMode::Normal => fmt_expr(f, &arg.expr, level),
    }
}

fn fmt_function(f: &mut fmt::Formatter<'_>, func: &Function, level: usize) -> fmt::Result {
    indent(f, level)?;
    write!(f, "Function sym:{}", func.name.name.as_u32())?;
    if !func.params.is_empty() {
        write!(f, "(")?;
        for (i, param) in func.params.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            fmt_param(f, param)?;
        }
        write!(f, ")")?;
    }
    if let Some(ref ret) = func.return_type {
        write!(f, " -> {}", ret)?;
    }
    writeln!(f)?;
    fmt_expr(f, &func.body, level + 1)?;
    Ok(())
}

fn fmt_expr(f: &mut fmt::Formatter<'_>, expr: &Expr, level: usize) -> fmt::Result {
    indent(f, level)?;
    match expr {
        Expr::Int(lit) => writeln!(f, "Int({})", lit.value),
        Expr::String(lit) => writeln!(f, "String(sym:{})", lit.value.as_u32()),
        Expr::Bool(lit) => writeln!(f, "Bool({})", lit.value),
        Expr::Unit(_) => writeln!(f, "Unit"),
        Expr::Ident(ident) => writeln!(f, "Ident(sym:{})", ident.name.as_u32()),
        Expr::Binary(bin) => {
            writeln!(f, "Binary {:?}", bin.op)?;
            fmt_expr(f, &bin.left, level + 1)?;
            fmt_expr(f, &bin.right, level + 1)
        }
        Expr::Unary(un) => {
            writeln!(f, "Unary {:?}", un.op)?;
            fmt_expr(f, &un.operand, level + 1)
        }
        Expr::Paren(paren) => {
            writeln!(f, "Paren")?;
            fmt_expr(f, &paren.inner, level + 1)
        }
        Expr::Block(block) => {
            writeln!(f, "Block")?;
            for stmt in &block.statements {
                fmt_stmt(f, stmt, level + 1)?;
            }
            fmt_expr(f, &block.expr, level + 1)
        }
        Expr::If(if_expr) => {
            writeln!(f, "If")?;
            indent(f, level + 1)?;
            writeln!(f, "Cond:")?;
            fmt_expr(f, &if_expr.cond, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Then:")?;
            fmt_block_expr(f, &if_expr.then_block, level + 2)?;
            if let Some(ref else_block) = if_expr.else_block {
                indent(f, level + 1)?;
                writeln!(f, "Else:")?;
                fmt_block_expr(f, else_block, level + 2)?;
            }
            Ok(())
        }
        Expr::Match(match_expr) => {
            writeln!(f, "Match")?;
            indent(f, level + 1)?;
            writeln!(f, "Scrutinee:")?;
            fmt_expr(f, &match_expr.scrutinee, level + 2)?;
            for arm in &match_expr.arms {
                indent(f, level + 1)?;
                writeln!(f, "Arm {:?} =>", arm.pattern)?;
                fmt_expr(f, &arm.body, level + 2)?;
            }
            Ok(())
        }
        Expr::While(while_expr) => {
            writeln!(f, "While")?;
            indent(f, level + 1)?;
            writeln!(f, "Cond:")?;
            fmt_expr(f, &while_expr.cond, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Body:")?;
            fmt_block_expr(f, &while_expr.body, level + 2)
        }
        Expr::Loop(loop_expr) => {
            writeln!(f, "Loop")?;
            fmt_block_expr(f, &loop_expr.body, level + 1)
        }
        Expr::Call(call) => {
            writeln!(f, "Call sym:{}", call.name.name.as_u32())?;
            for arg in &call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::IntrinsicCall(intrinsic) => {
            writeln!(f, "Intrinsic @sym:{}", intrinsic.name.name.as_u32())?;
            for arg in &intrinsic.args {
                match arg {
                    IntrinsicArg::Expr(expr) => fmt_expr(f, expr, level + 1)?,
                    IntrinsicArg::Type(ty) => {
                        indent(f, level + 1)?;
                        writeln!(f, "Type {:?}", ty)?;
                    }
                }
            }
            Ok(())
        }
        Expr::Break(_) => writeln!(f, "Break"),
        Expr::Continue(_) => writeln!(f, "Continue"),
        Expr::Return(ret) => {
            if let Some(ref value) = ret.value {
                writeln!(f, "Return")?;
                fmt_expr(f, value, level + 1)
            } else {
                writeln!(f, "Return (unit)")
            }
        }
        Expr::StructLit(lit) => {
            writeln!(f, "StructLit sym:{}", lit.name.name.as_u32())?;
            for field in &lit.fields {
                indent(f, level + 1)?;
                writeln!(f, "sym:{} =", field.name.name.as_u32())?;
                fmt_expr(f, &field.value, level + 2)?;
            }
            Ok(())
        }
        Expr::Field(field) => {
            writeln!(f, "Field .sym:{}", field.field.name.as_u32())?;
            fmt_expr(f, &field.base, level + 1)
        }
        Expr::MethodCall(method_call) => {
            writeln!(f, "MethodCall .sym:{}", method_call.method.name.as_u32())?;
            indent(f, level + 1)?;
            writeln!(f, "Receiver:")?;
            fmt_expr(f, &method_call.receiver, level + 2)?;
            if !method_call.args.is_empty() {
                indent(f, level + 1)?;
                writeln!(f, "Args:")?;
                for arg in &method_call.args {
                    fmt_call_arg(f, arg, level + 2)?;
                }
            }
            Ok(())
        }
        Expr::ArrayLit(array) => {
            writeln!(f, "ArrayLit")?;
            for elem in &array.elements {
                fmt_expr(f, elem, level + 1)?;
            }
            Ok(())
        }
        Expr::Index(index) => {
            writeln!(f, "Index")?;
            indent(f, level + 1)?;
            writeln!(f, "Base:")?;
            fmt_expr(f, &index.base, level + 2)?;
            indent(f, level + 1)?;
            writeln!(f, "Index:")?;
            fmt_expr(f, &index.index, level + 2)
        }
        Expr::Path(path) => writeln!(
            f,
            "Path sym:{}::sym:{}",
            path.type_name.name.as_u32(),
            path.variant.name.as_u32()
        ),
        Expr::AssocFnCall(assoc_fn_call) => {
            writeln!(
                f,
                "AssocFnCall sym:{}::sym:{}",
                assoc_fn_call.type_name.name.as_u32(),
                assoc_fn_call.function.name.as_u32()
            )?;
            for arg in &assoc_fn_call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::SelfExpr(_) => {
            writeln!(f, "SelfExpr")
        }
    }
}

fn fmt_block_expr(f: &mut fmt::Formatter<'_>, block: &BlockExpr, level: usize) -> fmt::Result {
    for stmt in &block.statements {
        fmt_stmt(f, stmt, level)?;
    }
    fmt_expr(f, &block.expr, level)
}

fn fmt_stmt(f: &mut fmt::Formatter<'_>, stmt: &Statement, level: usize) -> fmt::Result {
    indent(f, level)?;
    match stmt {
        Statement::Let(let_stmt) => {
            write!(f, "Let")?;
            if let_stmt.is_mut {
                write!(f, " mut")?;
            }
            match &let_stmt.pattern {
                LetPattern::Ident(ident) => write!(f, " sym:{}", ident.name.as_u32())?,
                LetPattern::Wildcard(_) => write!(f, " _")?,
            }
            if let Some(ref ty) = let_stmt.ty {
                write!(f, ": {}", ty)?;
            }
            writeln!(f)?;
            fmt_expr(f, &let_stmt.init, level + 1)
        }
        Statement::Assign(assign) => {
            match &assign.target {
                AssignTarget::Var(ident) => writeln!(f, "Assign sym:{}", ident.name.as_u32())?,
                AssignTarget::Field(field) => {
                    writeln!(f, "Assign field .sym:{}", field.field.name.as_u32())?;
                    fmt_expr(f, &field.base, level + 1)?;
                }
                AssignTarget::Index(index) => {
                    writeln!(f, "Assign index")?;
                    indent(f, level + 1)?;
                    writeln!(f, "Base:")?;
                    fmt_expr(f, &index.base, level + 2)?;
                    indent(f, level + 1)?;
                    writeln!(f, "Index:")?;
                    fmt_expr(f, &index.index, level + 2)?;
                }
            }
            fmt_expr(f, &assign.value, level + 1)
        }
        Statement::Expr(expr) => {
            writeln!(f, "ExprStmt")?;
            fmt_expr(f, expr, level + 1)
        }
    }
}
