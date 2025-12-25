//! Parser and AST for the Rue programming language.
//!
//! Uses chumsky for parser combinators with Pratt parsing for expressions.

pub mod ast;
mod chumsky_parser;

pub use ast::{
    ArrayLitExpr, AssignStatement, AssignTarget, Ast, BinaryExpr, BinaryOp, BlockExpr, CallExpr,
    EnumDecl, EnumVariant, Expr, FieldDecl, FieldExpr, FieldInit, Function, Ident, IndexExpr,
    IntLit, IntrinsicArg, IntrinsicCallExpr, Item, LetPattern, LetStatement, MatchArm, MatchExpr,
    Param, ParenExpr, PathExpr, PathPattern, Pattern, ReturnExpr, Statement, StructDecl,
    StructLitExpr, TypeExpr, UnaryExpr, UnaryOp, WhileExpr,
};
pub use chumsky_parser::ChumskyParser as Parser;
