//! Parser and AST for the Rue programming language.

pub mod ast;
pub mod intrinsics;
mod parser;
mod parser_policy;
mod validate;

pub use validate::KNOWN_DIRECTIVES;

/// Maximum number of detailed parser diagnostics retained for one source file.
/// If more unique grammar-recovery or post-parse validation diagnostics are
/// produced, the parser appends one
/// [`rue_error::ErrorKind::ParserDiagnosticsOmitted`] summary.
///
/// The budget is local to each [`Parser`] invocation. The preceding lexer phase
/// uses its own independent per-file diagnostic budget.
pub const PARSER_DIAGNOSTIC_BUDGET: usize = 100;

pub use ast::{
    ArgMode,
    ArrayLength,
    ArrayLitExpr,
    AssignStatement,
    AssignTarget,
    Ast,
    BinaryExpr,
    BinaryOp,
    BlockExpr,
    CallArg,
    CallExpr,
    Directive,
    DirectiveArg,
    EnumDecl,
    EnumVariant,
    Expr,
    FieldDecl,
    FieldExpr,
    FieldInit,
    ForExpr,
    Function,
    Ident,
    IndexExpr,
    IntLit,
    IntrinsicArg,
    IntrinsicCallExpr,
    Item,
    LetPattern,
    LetStatement,
    MatchArm,
    MatchExpr,
    Method,
    MethodCallExpr,
    // Struct-of-arrays AST types
    Param,
    ParamMode,
    ParenExpr,
    PathExpr,
    PathPattern,
    Pattern,
    ReturnExpr,
    SelfParam,
    Statement,
    StructDecl,
    StructLitExpr,
    TypeExpr,
    TypeLitExpr,
    UnaryExpr,
    UnaryOp,
    WhileExpr,
};
pub use parser::Parser;
