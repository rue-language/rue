//! Parser and AST for the Rue programming language.

pub mod ast;
pub mod directives;
pub mod intrinsics;
mod parser;
mod parser_policy;
mod validate;

/// Maximum number of detailed parser diagnostics retained for one source file.
/// If more unique grammar-recovery or post-parse validation diagnostics are
/// produced, the parser appends one
/// [`rue_error::ErrorKind::ParserDiagnosticsOmitted`] summary.
///
/// The budget is local to each [`Parser`] invocation. The preceding lexer phase
/// uses its own independent per-file diagnostic budget.
pub const PARSER_DIAGNOSTIC_BUDGET: usize = 100;

pub use directives::{
    DirectiveArgValue, DirectiveArity, DirectiveName, DirectiveSite, ReprArg, WarningName,
};

pub use ast::{
    AnonymousStructMetadata,
    ArgMode,
    ArrayLength,
    ArrayLitExpr,
    AssignStatement,
    AssignTarget,
    AssocTypeDecl,
    AssocTypeRequirement,
    Ast,
    BinaryExpr,
    BinaryOp,
    BlockExpr,
    CallArg,
    CallExpr,
    CompoundOp,
    ConformanceDecl,
    Directive,
    DirectiveArg,
    EnumDecl,
    EnumVariant,
    Expr,
    FieldDecl,
    FieldExpr,
    FieldInit,
    FnTypeParam,
    ForExpr,
    Function,
    Ident,
    IndexExpr,
    IntLit,
    InterfaceDecl,
    InterfaceRequirement,
    IntrinsicArg,
    IntrinsicCallExpr,
    Item,
    LetPattern,
    LetStatement,
    MatchArm,
    MatchExpr,
    Method,
    MethodCallExpr,
    MethodSig,
    // Struct-of-arrays AST types
    Param,
    ParamMode,
    ParenExpr,
    PathPattern,
    Pattern,
    PatternElement,
    ReturnExpr,
    SelfParam,
    Statement,
    StructDecl,
    StructLitExpr,
    StructPattern,
    StructPatternBinding,
    StructPatternField,
    TypeExpr,
    TypeLitExpr,
    UnaryExpr,
    UnaryOp,
    WhileExpr,
};
pub use parser::Parser;
