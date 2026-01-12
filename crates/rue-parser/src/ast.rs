//! Abstract Syntax Tree types for Rue.
//!
//! The AST represents the syntactic structure of the source code.
//! It closely mirrors the source syntax and preserves all information
//! needed for error reporting.
//!
//! ## SmallVec Usage
//!
//! Some non-recursive Vec fields use SmallVec to avoid heap allocation for
//! common small sizes:
//! - `Directives` (SmallVec<[Directive; 1]>) - most items have 0-1 directives
//!
//! ## Vec Usage (Cannot Use SmallVec)
//!
//! Vec fields containing recursive types (Expr) cannot use SmallVec because
//! Expr's size cannot be determined at compile time. These include:
//! - `Vec<CallArg>` - CallArg contains Expr
//! - `Vec<MatchArm>` - contains Expr
//! - `Vec<FieldInit>` - contains Box<Expr>
//! - `Vec<IntrinsicArg>` - contains Expr
//! - `Vec<Statement>` - Statement contains Expr
//! - `Vec<Expr>` - directly recursive
//!
//! The IR layers (RIR, AIR, CFG) use index-based references which avoid
//! this issue and are already efficiently allocated.

use std::fmt;

use lasso::{Key, Spur};
use rue_span::Span;
use smallvec::SmallVec;

/// Type alias for a small vector of directives.
/// Most items have 0-1 directives, so we inline capacity for 1.
pub type Directives = SmallVec<[Directive; 1]>;

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
    DropFn(DropFn),
    /// Constant declaration (e.g., `const math = @import("math");`)
    Const(ConstDecl),
    /// Error node for recovered parse errors at item level.
    /// Used by error recovery to continue parsing after a syntax error.
    Error(Span),
}

/// A constant declaration.
///
/// Constants are compile-time values. In the context of the module system,
/// they're used for re-exports:
/// ```rue
/// // _utils.rue (directory module root)
/// pub const strings = @import("utils/strings.rue");
/// pub const helper = @import("utils/internal.rue").helper;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstDecl {
    /// Directives applied to this const
    pub directives: Directives,
    /// Visibility of this constant
    pub visibility: Visibility,
    /// Constant name
    pub name: Ident,
    /// Optional type annotation (usually inferred)
    pub ty: Option<TypeExpr>,
    /// Initializer expression
    pub init: Box<Expr>,
    /// Span covering the entire const declaration
    pub span: Span,
}

/// A struct declaration.
///
/// Structs can contain both fields and methods. Methods are defined inline
/// within the struct block, not in separate impl blocks.
///
/// ```rue
/// struct Point {
///     x: i32,
///     y: i32,
///
///     fn distance(self) -> i32 {
///         self.x + self.y
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    /// Directives applied to this struct (e.g., @copy)
    pub directives: Directives,
    /// Visibility of this struct
    pub visibility: Visibility,
    /// Whether this struct is a linear type (must be consumed, cannot be dropped)
    pub is_linear: bool,
    /// Struct name
    pub name: Ident,
    /// Struct fields
    pub fields: Vec<FieldDecl>,
    /// Methods defined on this struct
    pub methods: Vec<Method>,
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
    /// Visibility of this enum
    pub visibility: Visibility,
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
    pub directives: Directives,
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

/// Visibility of an item (function, struct, enum, etc.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Private to the current file (default)
    #[default]
    Private,
    /// Public - visible to importers
    Public,
}

/// A function definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// Directives applied to this function
    pub directives: Directives,
    /// Visibility of this function
    pub visibility: Visibility,
    /// Whether this function is marked `unchecked` (can only be called from checked blocks)
    pub is_unchecked: bool,
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
    /// Comptime parameter - evaluated at compile time (used for type parameters)
    Comptime,
}

impl Default for ParamMode {
    fn default() -> Self {
        ParamMode::Normal
    }
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Whether this parameter is evaluated at compile time
    pub is_comptime: bool,
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
    pub name: Spur,
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
    /// Anonymous struct type: struct { field: Type, fn method(...) { ... }, ... }
    /// Used in comptime type construction (e.g., `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`)
    /// Methods can be included inside the struct definition (Zig-style).
    AnonymousStruct {
        /// Field declarations (name and type)
        fields: Vec<AnonStructField>,
        /// Method definitions inside the anonymous struct
        methods: Vec<Method>,
        span: Span,
    },
    /// Raw pointer to immutable data: ptr const T
    PointerConst { pointee: Box<TypeExpr>, span: Span },
    /// Raw pointer to mutable data: ptr mut T
    PointerMut { pointee: Box<TypeExpr>, span: Span },
}

/// A field in an anonymous struct type expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonStructField {
    /// Field name
    pub name: Ident,
    /// Field type
    pub ty: TypeExpr,
    /// Span covering the entire field declaration
    pub span: Span,
}

impl TypeExpr {
    /// Get the span of this type expression.
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named(ident) => ident.span,
            TypeExpr::Unit(span) => *span,
            TypeExpr::Never(span) => *span,
            TypeExpr::Array { span, .. } => *span,
            TypeExpr::AnonymousStruct { span, .. } => *span,
            TypeExpr::PointerConst { span, .. } => *span,
            TypeExpr::PointerMut { span, .. } => *span,
        }
    }
}

impl fmt::Display for TypeExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeExpr::Named(ident) => write!(f, "sym:{}", ident.name.into_usize()),
            TypeExpr::Unit(_) => write!(f, "()"),
            TypeExpr::Never(_) => write!(f, "!"),
            TypeExpr::Array {
                element, length, ..
            } => write!(f, "[{}; {}]", element, length),
            TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                write!(f, "struct {{ ")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "sym:{}: {}", field.name.name.into_usize(), field.ty)?;
                }
                for (i, method) in methods.iter().enumerate() {
                    if !fields.is_empty() || i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "fn sym:{}", method.name.name.into_usize())?;
                }
                write!(f, " }}")
            }
            TypeExpr::PointerConst { pointee, .. } => write!(f, "ptr const {}", pointee),
            TypeExpr::PointerMut { pointee, .. } => write!(f, "ptr mut {}", pointee),
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
    /// Comptime block expression (e.g., `comptime { 1 + 2 }`)
    Comptime(ComptimeBlockExpr),
    /// Checked block expression (e.g., `checked { @ptr_read(p) }`)
    Checked(CheckedBlockExpr),
    /// Type literal expression (e.g., `i32` used as a value in generic function calls)
    TypeLit(TypeLitExpr),
    /// Error node for recovered parse errors.
    /// Used by error recovery to continue parsing after a syntax error.
    Error(Span),
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
    pub value: Spur,
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

/// A path pattern (e.g., `Color::Red` or `module.Color::Red` for enum variant matching).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPattern {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Color::Red`)
    pub base: Option<Box<Expr>>,
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

/// A struct literal expression (e.g., `Point { x: 1, y: 2 }` or `module.Point { x: 1, y: 2 }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLitExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Point { ... }`)
    pub base: Option<Box<Expr>>,
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

/// A path expression (e.g., `Color::Red` or `module.Color::Red` for enum variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Color::Red`)
    pub base: Option<Box<Expr>>,
    /// The type name (e.g., `Color`)
    pub type_name: Ident,
    /// The variant name (e.g., `Red`)
    pub variant: Ident,
    pub span: Span,
}

/// An associated function call expression (e.g., `Point::origin()` or `module.Point::origin()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssocFnCallExpr {
    /// Optional module/namespace prefix (e.g., `utils` in `utils.Point::origin()`)
    pub base: Option<Box<Expr>>,
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
    pub directives: Directives,
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

/// A comptime block expression (e.g., `comptime { 1 + 2 }`).
/// The expression inside must be evaluable at compile time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComptimeBlockExpr {
    /// The expression to evaluate at compile time
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A checked block expression (e.g., `checked { @ptr_read(p) }`).
/// Unchecked operations (raw pointer manipulation, calling unchecked functions)
/// are only allowed inside checked blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedBlockExpr {
    /// The expression inside the checked block
    pub expr: Box<Expr>,
    pub span: Span,
}

/// A type literal expression (e.g., `i32` used as a value).
/// This represents a type used as a value in expression context, typically
/// as an argument to a generic function with comptime parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeLitExpr {
    /// The type being used as a value
    pub type_expr: TypeExpr,
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
            Expr::Comptime(comptime_expr) => comptime_expr.span,
            Expr::Checked(checked_expr) => checked_expr.span,
            Expr::TypeLit(type_lit) => type_lit.span,
            Expr::Error(span) => *span,
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
                Item::DropFn(drop_fn) => fmt_drop_fn(f, drop_fn, 0)?,
                Item::Const(c) => fmt_const(f, c, 0)?,
                Item::Error(span) => writeln!(f, "Error({:?})", span)?,
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
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    if s.is_linear {
        write!(f, "linear ")?;
    }
    writeln!(f, "Struct sym:{}", s.name.name.into_usize())?;
    for field in &s.fields {
        indent(f, level + 1)?;
        writeln!(
            f,
            "Field sym:{} : {}",
            field.name.name.into_usize(),
            field.ty
        )?;
    }
    for method in &s.methods {
        fmt_method(f, method, level + 1)?;
    }
    Ok(())
}

fn fmt_enum(f: &mut fmt::Formatter<'_>, e: &EnumDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(f, "Enum sym:{}", e.name.name.into_usize())?;
    for variant in &e.variants {
        indent(f, level + 1)?;
        writeln!(f, "Variant sym:{}", variant.name.name.into_usize())?;
    }
    Ok(())
}

fn fmt_const(f: &mut fmt::Formatter<'_>, c: &ConstDecl, level: usize) -> fmt::Result {
    indent(f, level)?;
    for directive in &c.directives {
        write!(f, "@sym:{} ", directive.name.name.into_usize())?;
    }
    if c.visibility == Visibility::Public {
        write!(f, "pub ")?;
    }
    write!(f, "Const sym:{}", c.name.name.into_usize())?;
    if let Some(ref ty) = c.ty {
        write!(f, ": {}", ty)?;
    }
    writeln!(f)?;
    fmt_expr(f, &c.init, level + 1)?;
    Ok(())
}

fn fmt_drop_fn(f: &mut fmt::Formatter<'_>, drop_fn: &DropFn, level: usize) -> fmt::Result {
    indent(f, level)?;
    writeln!(
        f,
        "DropFn sym:{}(self)",
        drop_fn.type_name.name.into_usize()
    )?;
    fmt_expr(f, &drop_fn.body, level + 1)?;
    Ok(())
}

fn fmt_method(f: &mut fmt::Formatter<'_>, method: &Method, level: usize) -> fmt::Result {
    indent(f, level)?;
    write!(f, "Method sym:{}", method.name.name.into_usize())?;
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
        ParamMode::Comptime => write!(f, "comptime ")?,
        ParamMode::Normal => {}
    }
    write!(f, "sym:{}: {}", param.name.name.into_usize(), param.ty)
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
    if func.is_unchecked {
        write!(f, "unchecked ")?;
    }
    write!(f, "Function sym:{}", func.name.name.into_usize())?;
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
        Expr::String(lit) => writeln!(f, "String(sym:{})", lit.value.into_usize()),
        Expr::Bool(lit) => writeln!(f, "Bool({})", lit.value),
        Expr::Unit(_) => writeln!(f, "Unit"),
        Expr::Ident(ident) => writeln!(f, "Ident(sym:{})", ident.name.into_usize()),
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
            writeln!(f, "Call sym:{}", call.name.name.into_usize())?;
            for arg in &call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::IntrinsicCall(intrinsic) => {
            writeln!(f, "Intrinsic @sym:{}", intrinsic.name.name.into_usize())?;
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
            writeln!(f, "StructLit sym:{}", lit.name.name.into_usize())?;
            for field in &lit.fields {
                indent(f, level + 1)?;
                writeln!(f, "sym:{} =", field.name.name.into_usize())?;
                fmt_expr(f, &field.value, level + 2)?;
            }
            Ok(())
        }
        Expr::Field(field) => {
            writeln!(f, "Field .sym:{}", field.field.name.into_usize())?;
            fmt_expr(f, &field.base, level + 1)
        }
        Expr::MethodCall(method_call) => {
            writeln!(
                f,
                "MethodCall .sym:{}",
                method_call.method.name.into_usize()
            )?;
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
            path.type_name.name.into_usize(),
            path.variant.name.into_usize()
        ),
        Expr::AssocFnCall(assoc_fn_call) => {
            writeln!(
                f,
                "AssocFnCall sym:{}::sym:{}",
                assoc_fn_call.type_name.name.into_usize(),
                assoc_fn_call.function.name.into_usize()
            )?;
            for arg in &assoc_fn_call.args {
                fmt_call_arg(f, arg, level + 1)?;
            }
            Ok(())
        }
        Expr::SelfExpr(_) => {
            writeln!(f, "SelfExpr")
        }
        Expr::Comptime(comptime) => {
            writeln!(f, "Comptime")?;
            fmt_expr(f, &comptime.expr, level + 1)
        }
        Expr::Checked(checked) => {
            writeln!(f, "Checked")?;
            fmt_expr(f, &checked.expr, level + 1)
        }
        Expr::TypeLit(type_lit) => {
            writeln!(f, "TypeLit({})", type_lit.type_expr)
        }
        Expr::Error(span) => {
            writeln!(f, "Error({:?})", span)
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
                LetPattern::Ident(ident) => write!(f, " sym:{}", ident.name.into_usize())?,
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
                AssignTarget::Var(ident) => writeln!(f, "Assign sym:{}", ident.name.into_usize())?,
                AssignTarget::Field(field) => {
                    writeln!(f, "Assign field .sym:{}", field.field.name.into_usize())?;
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

// ============================================================================
// Struct-of-Arrays (SOA) AST Layout
// ============================================================================
//
// This section implements Zig-style SOA layout for the AST.
// See docs/designs/soa-ast-layout.md for full design rationale.
//
// Key characteristics:
// - Fixed-size nodes (tag + main_token + lhs + rhs)
// - Index-based references (no lifetimes)
// - Extra data array for nodes with >2 children
// - Single allocation for entire AST
// - Better cache locality than tree-based approach
//
// Migration: This will eventually replace the tree-based Ast above.
// For now, both representations coexist during Phase 2-3 migration.

/// Node index - references a node in the SOA AST.
///
/// Nodes are stored in parallel arrays (tags, data, extra) and referenced
/// by their index. This is similar to how RIR uses InstRef.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIndex(pub u32);

impl NodeIndex {
    /// Create a new node index.
    pub const fn new(idx: u32) -> Self {
        NodeIndex(idx)
    }

    /// Get the raw index value.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Get the index as usize for array indexing.
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Sentinel value representing "no node" or "null node".
/// Used for optional children (e.g., else block in if expression).
pub const NULL_NODE: NodeIndex = NodeIndex(u32::MAX);

/// Encode a UnaryOp into a u32 for storage in NodeData.
pub fn encode_unary_op(op: UnaryOp) -> u32 {
    match op {
        UnaryOp::Neg => 0,
        UnaryOp::Not => 1,
        UnaryOp::BitNot => 2,
    }
}

/// Node tag - identifies what kind of node this is.
///
/// The tag determines how to interpret the lhs/rhs fields in NodeData.
/// See docs/designs/soa-ast-layout.md for encoding details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeTag {
    // ===== Items (top-level declarations) =====
    /// Function declaration: fn name(params) -> ret { body }
    /// - lhs: index into extra (param count + param nodes)
    /// - rhs: body expression node
    Function,

    /// Struct declaration: struct Name { fields... methods... }
    /// - lhs: index into extra (field count + field nodes)
    /// - rhs: index into extra (method count + method nodes)
    StructDecl,

    /// Enum declaration: enum Name { variants... }
    /// - lhs: index into extra (variant count + variant nodes)
    /// - rhs: 0 (unused)
    EnumDecl,

    /// Drop function: drop fn TypeName(self) { body }
    /// - lhs: type name identifier
    /// - rhs: body expression node
    DropFn,

    /// Constant declaration: const name: type = init;
    /// - lhs: type expression node (or NULL_NODE if inferred)
    /// - rhs: initializer expression node
    ConstDecl,

    // ===== Expressions - Literals =====
    /// Integer literal: 42
    /// - lhs: low 32 bits of u64 value
    /// - rhs: high 32 bits of u64 value
    IntLit,

    /// String literal: "hello"
    /// - lhs: Spur index (u32) for interned string
    /// - rhs: 0 (unused)
    StringLit,

    /// Boolean literal: true, false
    /// - lhs: 0 (false) or 1 (true)
    /// - rhs: 0 (unused)
    BoolLit,

    /// Unit literal: ()
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    UnitLit,

    // ===== Expressions - Identifiers and Paths =====
    /// Identifier: variable_name
    /// - lhs: Spur index (u32) for identifier name
    /// - rhs: 0 (unused)
    Ident,

    /// Path expression: Color::Red
    /// - lhs: type name identifier node
    /// - rhs: variant name identifier node
    Path,

    // ===== Expressions - Operations =====
    /// Unary expression: -x, !x, ~x
    /// - lhs: operand expression node
    /// - rhs: operator kind (u32 from UnaryOp enum)
    UnaryExpr,

    /// Parenthesized expression: (expr)
    /// - lhs: inner expression node
    /// - rhs: 0 (unused)
    ParenExpr,

    /// Binary expression: a + b, a == b, etc.
    /// - main_token: the operator token
    /// - lhs: left operand expression node
    /// - rhs: right operand expression node
    BinaryExpr,

    // ===== Expressions - Control Flow =====
    /// If expression: if cond { then } else { else_block }
    /// - lhs: condition expression node
    /// - rhs: index into extra (then_block, else_block or NULL_NODE)
    IfExpr,

    /// Match expression: match x { arms... }
    /// - lhs: scrutinee expression node
    /// - rhs: index into extra (arm count + arm nodes)
    MatchExpr,

    /// While loop: while cond { body }
    /// - lhs: condition expression node
    /// - rhs: body block expression node
    WhileExpr,

    /// Infinite loop: loop { body }
    /// - lhs: body block expression node
    /// - rhs: 0 (unused)
    LoopExpr,

    /// Break statement: break
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    BreakExpr,

    /// Continue statement: continue
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    ContinueExpr,

    /// Return statement: return expr
    /// - lhs: value expression node (or NULL_NODE for implicit unit return)
    /// - rhs: 0 (unused)
    ReturnExpr,

    // ===== Expressions - Blocks and Statements =====
    /// Block expression: { stmts...; final_expr }
    /// - lhs: index into extra (stmt count + stmt nodes)
    /// - rhs: final expression node
    BlockExpr,

    /// Let statement: let x: type = init;
    /// - lhs: pattern node (identifier or wildcard)
    /// - rhs: index into extra (flags, type_expr or NULL_NODE, init_expr)
    LetStmt,

    /// Assignment statement: x = value;
    /// - lhs: target node (Ident, FieldExpr, or IndexExpr)
    /// - rhs: value expression node
    AssignStmt,

    /// Expression statement: expr;
    /// - lhs: expression node
    /// - rhs: 0 (unused)
    ExprStmt,

    // ===== Expressions - Function Calls =====
    /// Function call: func(args...)
    /// - lhs: callee identifier node
    /// - rhs: index into extra (arg count + arg nodes)
    Call,

    /// Method call: receiver.method(args...)
    /// - lhs: receiver expression node
    /// - rhs: index into extra (method name, arg count, arg nodes)
    MethodCall,

    /// Intrinsic call: @intrinsic(args...)
    /// - lhs: intrinsic name identifier node
    /// - rhs: index into extra (arg count + arg nodes)
    IntrinsicCall,

    /// Associated function call: Type::func(args...)
    /// - lhs: type name identifier node
    /// - rhs: index into extra (fn name, arg count, arg nodes)
    AssocFnCall,

    // ===== Expressions - Struct Operations =====
    /// Struct literal: Point { x: 1, y: 2 }
    /// - lhs: struct name identifier node
    /// - rhs: index into extra (field init count + field init nodes)
    StructLit,

    /// Field access: obj.field
    /// - lhs: base expression node
    /// - rhs: field name identifier node
    FieldExpr,

    /// Field initializer in struct literal: field_name: value
    /// - lhs: field name identifier node
    /// - rhs: value expression node
    FieldInit,

    // ===== Expressions - Array Operations =====
    /// Array literal: [1, 2, 3]
    /// - lhs: index into extra (element count + element nodes)
    /// - rhs: 0 (unused, count stored in extra)
    ArrayLit,

    /// Array index: arr[index]
    /// - lhs: base expression node
    /// - rhs: index expression node
    IndexExpr,

    // ===== Expressions - Special =====
    /// Self expression: self
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    SelfExpr,

    /// Comptime block: comptime { expr }
    /// - lhs: inner expression node
    /// - rhs: 0 (unused)
    ComptimeBlockExpr,

    /// Checked block: checked { expr }
    /// - lhs: inner expression node
    /// - rhs: 0 (unused)
    CheckedBlockExpr,

    /// Type literal: i32 (used as value)
    /// - lhs: type expression node
    /// - rhs: 0 (unused)
    TypeLit,

    // ===== Type Expressions =====
    /// Named type: i32, MyStruct
    /// - lhs: name identifier node
    /// - rhs: 0 (unused)
    TypeNamed,

    /// Unit type: ()
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    TypeUnit,

    /// Never type: !
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    TypeNever,

    /// Array type: [T; N]
    /// - lhs: element type expression node
    /// - rhs: length (u32, stored directly)
    TypeArray,

    /// Anonymous struct type: struct { fields... methods... }
    /// - lhs: index into extra (field count + field nodes)
    /// - rhs: index into extra (method count + method nodes)
    TypeAnonStruct,

    /// Const pointer type: ptr const T
    /// - lhs: pointee type expression node
    /// - rhs: 0 (unused)
    TypePointerConst,

    /// Mutable pointer type: ptr mut T
    /// - lhs: pointee type expression node
    /// - rhs: 0 (unused)
    TypePointerMut,

    // ===== Patterns =====
    /// Wildcard pattern: _
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    PatternWildcard,

    /// Integer literal pattern: 42, -1
    /// - lhs: low 32 bits of value
    /// - rhs: high 32 bits of value
    PatternInt,

    /// Boolean literal pattern: true, false
    /// - lhs: 0 (false) or 1 (true)
    /// - rhs: 0 (unused)
    PatternBool,

    /// Path pattern: Color::Red
    /// - lhs: type name identifier node
    /// - rhs: variant name identifier node
    PatternPath,

    // ===== Other Nodes =====
    /// Function parameter: name: type
    /// - lhs: name identifier node
    /// - rhs: type expression node
    /// - extra: flags (is_comptime, mode)
    Param,

    /// Method definition
    /// - lhs: index into extra (name, receiver?, param count, params)
    /// - rhs: index into extra (return_type or NULL_NODE, body_expr)
    Method,

    /// Match arm: pattern => body
    /// - lhs: pattern node
    /// - rhs: body expression node
    MatchArm,

    /// Call argument (wraps expression with mode flags)
    /// - lhs: expression node
    /// - rhs: flags (normal=0, inout=1, borrow=2)
    CallArg,

    /// Field declaration in struct
    /// - lhs: name identifier node
    /// - rhs: type expression node
    FieldDecl,

    /// Enum variant
    /// - lhs: name identifier node
    /// - rhs: 0 (unused, payload support future)
    EnumVariant,

    /// Directive: @name(args...)
    /// - lhs: name identifier node
    /// - rhs: index into extra (arg count + arg nodes)
    Directive,

    /// Directive argument (currently just identifiers)
    /// - lhs: identifier node
    /// - rhs: 0 (unused)
    DirectiveArg,

    // ===== Error Recovery =====
    /// Error node (parse error recovery)
    /// - lhs: 0 (unused)
    /// - rhs: 0 (unused)
    ErrorNode,
}

/// Fixed-size node data (12 bytes total).
///
/// Each node in the SOA AST has:
/// - A tag (stored in separate tags array)
/// - A main_token (for span information)
/// - Two u32 slots (lhs and rhs) whose meaning depends on the tag
///
/// This matches Zig's design: compact, cache-friendly, uniform size.
#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    /// Primary token for this node.
    ///
    /// Used for:
    /// - Span information in error messages
    /// - Operator tokens (for BinaryExpr, UnaryExpr)
    /// - Keyword tokens (for if, while, etc.)
    pub main_token: u32,

    /// Left child or first data slot.
    ///
    /// Interpretation depends on NodeTag - see NodeTag documentation.
    /// Common uses:
    /// - Left operand in binary expressions
    /// - Single child in unary expressions
    /// - Index into extra_data for multi-child nodes
    /// - Direct data storage (e.g., low 32 bits of u64)
    pub lhs: u32,

    /// Right child or second data slot.
    ///
    /// Interpretation depends on NodeTag - see NodeTag documentation.
    /// Common uses:
    /// - Right operand in binary expressions
    /// - Index into extra_data for multi-child nodes
    /// - Direct data storage (e.g., high 32 bits of u64)
    /// - Flags and small enums
    pub rhs: u32,
}

/// Struct-of-Arrays AST representation.
///
/// This is the new SOA-based AST that will replace the tree-based `Ast`.
/// During migration (Phases 2-3), both representations will coexist.
///
/// Design principles:
/// - All nodes stored in parallel arrays (tags, data, extra)
/// - Nodes reference children by index, not pointers
/// - Single allocation for entire AST (better cache locality)
/// - Cheap cloning (just clone Arc, not deep copy)
///
/// See docs/designs/soa-ast-layout.md for full design.
#[derive(Debug, Clone)]
pub struct SoaAst {
    /// Node tags (what kind of node is at each index).
    ///
    /// Index i contains the tag for node NodeIndex(i).
    /// Length of this vec == number of nodes in the AST.
    pub tags: Vec<NodeTag>,

    /// Node data (main_token + lhs + rhs for each node).
    ///
    /// Parallel array to tags - tags[i] and data[i] together describe node i.
    pub data: Vec<NodeData>,

    /// Extra data storage for nodes with >2 children.
    ///
    /// Nodes that can't fit their data in lhs+rhs store additional
    /// data here. The lhs or rhs field contains an index into this array.
    ///
    /// Layout is node-type specific - see NodeTag documentation.
    pub extra: Vec<u32>,

    /// Root nodes (top-level items in the source file).
    ///
    /// These are the entry points for traversing the AST.
    /// Each element is a NodeIndex pointing to a Function, StructDecl, etc.
    pub items: Vec<NodeIndex>,
}

impl SoaAst {
    /// Create a new empty SOA AST.
    pub fn new() -> Self {
        SoaAst {
            tags: Vec::new(),
            data: Vec::new(),
            extra: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Create a new SOA AST with pre-allocated capacity.
    pub fn with_capacity(nodes: usize, extra: usize) -> Self {
        SoaAst {
            tags: Vec::with_capacity(nodes),
            data: Vec::with_capacity(nodes),
            extra: Vec::with_capacity(extra),
            items: Vec::new(),
        }
    }

    /// Get the tag for a node.
    pub fn node_tag(&self, idx: NodeIndex) -> NodeTag {
        self.tags[idx.as_usize()]
    }

    /// Get the data for a node.
    pub fn node_data(&self, idx: NodeIndex) -> NodeData {
        self.data[idx.as_usize()]
    }

    /// Get the main token for a node.
    pub fn main_token(&self, idx: NodeIndex) -> u32 {
        self.data[idx.as_usize()].main_token
    }

    /// Get the number of nodes in the AST.
    pub fn node_count(&self) -> usize {
        self.tags.len()
    }

    /// Get a slice of the extra data array.
    pub fn extra_slice(&self, start: usize, len: usize) -> &[u32] {
        &self.extra[start..start + len]
    }

    // ===== Typed Accessors =====
    // These provide type-safe access to specific node types.

    /// Get the value of an integer literal.
    pub fn int_value(&self, idx: NodeIndex) -> u64 {
        debug_assert_eq!(self.node_tag(idx), NodeTag::IntLit);
        let data = self.node_data(idx);
        (data.lhs as u64) | ((data.rhs as u64) << 32)
    }

    /// Get the boolean value of a boolean literal.
    pub fn bool_value(&self, idx: NodeIndex) -> bool {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BoolLit);
        let data = self.node_data(idx);
        data.lhs != 0
    }

    /// Get the string spur of a string literal.
    pub fn string_spur(&self, idx: NodeIndex) -> Spur {
        debug_assert_eq!(self.node_tag(idx), NodeTag::StringLit);
        let data = self.node_data(idx);
        Spur::try_from_usize(data.lhs as usize).expect("invalid spur")
    }

    /// Get the identifier spur.
    pub fn ident_spur(&self, idx: NodeIndex) -> Spur {
        debug_assert_eq!(self.node_tag(idx), NodeTag::Ident);
        let data = self.node_data(idx);
        Spur::try_from_usize(data.lhs as usize).expect("invalid spur")
    }

    /// Get the operands of a binary expression.
    pub fn binary_operands(&self, idx: NodeIndex) -> (NodeIndex, NodeIndex) {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BinaryExpr);
        let data = self.node_data(idx);
        (NodeIndex(data.lhs), NodeIndex(data.rhs))
    }

    /// Get the operand of a unary expression.
    pub fn unary_operand(&self, idx: NodeIndex) -> NodeIndex {
        debug_assert_eq!(self.node_tag(idx), NodeTag::UnaryExpr);
        let data = self.node_data(idx);
        NodeIndex(data.lhs)
    }

    /// Get the operator kind of a unary expression.
    pub fn unary_op(&self, idx: NodeIndex) -> UnaryOp {
        debug_assert_eq!(self.node_tag(idx), NodeTag::UnaryExpr);
        let data = self.node_data(idx);
        match data.rhs {
            0 => UnaryOp::Neg,
            1 => UnaryOp::Not,
            2 => UnaryOp::BitNot,
            _ => panic!("invalid UnaryOp encoding: {}", data.rhs),
        }
    }
}

impl Default for SoaAst {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod soa_tests {
    use super::*;

    #[test]
    fn test_node_index() {
        let idx = NodeIndex::new(42);
        assert_eq!(idx.as_u32(), 42);
        assert_eq!(idx.as_usize(), 42);
    }

    #[test]
    fn test_null_node() {
        assert_eq!(NULL_NODE.as_u32(), u32::MAX);
    }

    #[test]
    fn test_soa_ast_creation() {
        let ast = SoaAst::new();
        assert_eq!(ast.node_count(), 0);
        assert_eq!(ast.tags.len(), 0);
        assert_eq!(ast.data.len(), 0);
        assert_eq!(ast.extra.len(), 0);
    }

    #[test]
    fn test_soa_ast_with_capacity() {
        let ast = SoaAst::with_capacity(100, 50);
        assert!(ast.tags.capacity() >= 100);
        assert!(ast.data.capacity() >= 100);
        assert!(ast.extra.capacity() >= 50);
    }

    #[test]
    fn test_int_literal_encoding() {
        let mut ast = SoaAst::new();

        // Add an integer literal node
        let value = 0x123456789ABCDEF0u64;
        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: (value & 0xFFFFFFFF) as u32,         // low 32 bits
            rhs: ((value >> 32) & 0xFFFFFFFF) as u32, // high 32 bits
        });

        let idx = NodeIndex(0);
        assert_eq!(ast.node_tag(idx), NodeTag::IntLit);
        assert_eq!(ast.int_value(idx), value);
    }

    #[test]
    fn test_bool_literal_encoding() {
        let mut ast = SoaAst::new();

        // Add true
        ast.tags.push(NodeTag::BoolLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 1,
            rhs: 0,
        });

        // Add false
        ast.tags.push(NodeTag::BoolLit);
        ast.data.push(NodeData {
            main_token: 1,
            lhs: 0,
            rhs: 0,
        });

        assert_eq!(ast.bool_value(NodeIndex(0)), true);
        assert_eq!(ast.bool_value(NodeIndex(1)), false);
    }

    #[test]
    fn test_binary_expr_encoding() {
        let mut ast = SoaAst::new();

        // Create: 1 + 2
        // First add the literals
        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 1,
            rhs: 0,
        });

        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 1,
            lhs: 2,
            rhs: 0,
        });

        // Then add the binary expression
        ast.tags.push(NodeTag::BinaryExpr);
        ast.data.push(NodeData {
            main_token: 2, // the '+' token
            lhs: 0,        // left operand (node 0)
            rhs: 1,        // right operand (node 1)
        });

        let binop_idx = NodeIndex(2);
        assert_eq!(ast.node_tag(binop_idx), NodeTag::BinaryExpr);

        let (left, right) = ast.binary_operands(binop_idx);
        assert_eq!(left, NodeIndex(0));
        assert_eq!(right, NodeIndex(1));
        assert_eq!(ast.int_value(left), 1);
        assert_eq!(ast.int_value(right), 2);
    }

    #[test]
    fn test_unary_expr_encoding() {
        let mut ast = SoaAst::new();

        // Create: -42
        // First add the literal
        ast.tags.push(NodeTag::IntLit);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 42,
            rhs: 0,
        });

        // Then add the unary expression
        ast.tags.push(NodeTag::UnaryExpr);
        ast.data.push(NodeData {
            main_token: 1,                      // the '-' token
            lhs: 0,                             // operand (node 0)
            rhs: encode_unary_op(UnaryOp::Neg), // operator kind
        });

        let unary_idx = NodeIndex(1);
        assert_eq!(ast.node_tag(unary_idx), NodeTag::UnaryExpr);

        let operand = ast.unary_operand(unary_idx);
        assert_eq!(operand, NodeIndex(0));
        assert_eq!(ast.int_value(operand), 42);
        assert_eq!(ast.unary_op(unary_idx), UnaryOp::Neg);
    }

    #[test]
    fn test_ident_encoding() {
        let mut ast = SoaAst::new();

        // Mock identifier with spur index 123
        ast.tags.push(NodeTag::Ident);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 123, // spur index
            rhs: 0,
        });

        let idx = NodeIndex(0);
        assert_eq!(ast.node_tag(idx), NodeTag::Ident);
        assert_eq!(ast.node_data(idx).lhs, 123);
    }

    #[test]
    fn test_extra_data_slice() {
        let mut ast = SoaAst::new();
        ast.extra = vec![10, 20, 30, 40, 50];

        let slice = ast.extra_slice(1, 3);
        assert_eq!(slice, &[20, 30, 40]);
    }

    #[test]
    fn test_items() {
        let mut ast = SoaAst::new();

        // Add two function nodes
        ast.tags.push(NodeTag::Function);
        ast.data.push(NodeData {
            main_token: 0,
            lhs: 0,
            rhs: 0,
        });

        ast.tags.push(NodeTag::Function);
        ast.data.push(NodeData {
            main_token: 1,
            lhs: 0,
            rhs: 0,
        });

        ast.items = vec![NodeIndex(0), NodeIndex(1)];

        assert_eq!(ast.items.len(), 2);
        assert_eq!(ast.node_tag(ast.items[0]), NodeTag::Function);
        assert_eq!(ast.node_tag(ast.items[1]), NodeTag::Function);
    }
}
