//! Parser and AST for the Rue programming language.
//!
//! Uses chumsky for parser combinators with Pratt parsing for expressions.

pub mod ast;
mod chumsky_parser;
mod validate;

pub use validate::KNOWN_DIRECTIVES;

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
pub use chumsky_parser::ChumskyParser as Parser;
