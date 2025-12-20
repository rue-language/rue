//! Parser and AST for the Rue programming language.
//!
//! Uses chumsky for parser combinators with Pratt parsing for expressions.

pub mod ast;
mod chumsky_parser;

pub use ast::{
    AssignStatement, AssignTarget, Ast, BinaryExpr, BinaryOp, BlockExpr, CallExpr, Expr, FieldDecl,
    FieldExpr, FieldInit, Function, Ident, IntLit, IntrinsicCallExpr, Item, LetStatement, MatchArm,
    MatchExpr, Param, ParenExpr, Pattern, ReturnExpr, Statement, StructDecl, StructLitExpr,
    UnaryExpr, UnaryOp, WhileExpr,
};
pub use chumsky_parser::ChumskyParser as Parser;
