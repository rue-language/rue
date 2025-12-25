//! Parser and AST for the Rue programming language.
//!
//! Uses chumsky for parser combinators with Pratt parsing for expressions.

pub mod ast;
mod chumsky_parser;

pub use ast::{
    ArrayLitExpr, AssignStatement, AssignTarget, Ast, BinaryExpr, BinaryOp, BlockExpr, CallExpr,
    Directive, DirectiveArg, EnumDecl, EnumVariant, Expr, FieldDecl, FieldExpr, FieldInit,
    Function, Ident, ImplBlock, IndexExpr, IntLit, IntrinsicArg, IntrinsicCallExpr, Item,
    LetPattern, LetStatement, MatchArm, MatchExpr, Method, MethodCallExpr, Param, ParenExpr,
    PathExpr, PathPattern, Pattern, ReturnExpr, SelfParam, Statement, StructDecl, StructLitExpr,
    TypeExpr, UnaryExpr, UnaryOp, WhileExpr,
};
pub use chumsky_parser::ChumskyParser as Parser;
