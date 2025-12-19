//! Chumsky-based parser for the Rue programming language.
//!
//! This module provides a parser implementation using chumsky combinators
//! with Pratt parsing for expression precedence.

use crate::ast::{
    AssignStatement, AssignTarget, Ast, BinaryExpr, BinaryOp, BlockExpr, BoolLit, BreakExpr,
    CallExpr, ContinueExpr, Expr, FieldDecl, FieldExpr, FieldInit, Function, Ident, IfExpr, IntLit,
    IntrinsicCallExpr, Item, LetStatement, Param, ParenExpr, Statement, StructDecl, StructLitExpr,
    UnaryExpr, UnaryOp, WhileExpr,
};
use chumsky::input::{Input as ChumskyInput, Stream, ValueInput};
use chumsky::pratt::{infix, left, prefix};
use chumsky::prelude::*;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_lexer::TokenKind;
use rue_span::Span;

/// Convert chumsky SimpleSpan to rue_span::Span
fn to_rue_span(span: SimpleSpan) -> Span {
    Span::new(span.start as u32, span.end as u32)
}

/// Parser that produces Ident from identifier tokens
fn ident_parser<'src, I>() -> impl Parser<'src, I, Ident, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    select! {
        TokenKind::Ident(name) = e => Ident {
            name,
            span: to_rue_span(e.span()),
        },
    }
}

/// Parser for function parameters: name: type
fn param_parser<'src, I>() -> impl Parser<'src, I, Param, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(ident_parser())
        .map_with(|(name, ty), e| Param {
            name,
            ty,
            span: to_rue_span(e.span()),
        })
}

/// Parser for struct field declarations: name: type
fn field_decl_parser<'src, I>()
-> impl Parser<'src, I, FieldDecl, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(ident_parser())
        .map_with(|(name, ty), e| FieldDecl {
            name,
            ty,
            span: to_rue_span(e.span()),
        })
}

/// Parser for comma-separated struct field declarations
fn field_decls_parser<'src, I>()
-> impl Parser<'src, I, Vec<FieldDecl>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    field_decl_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect()
}

/// Parser for comma-separated parameters
fn params_parser<'src, I>()
-> impl Parser<'src, I, Vec<Param>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    param_parser()
        .separated_by(just(TokenKind::Comma))
        .collect()
}

/// Parser for comma-separated expression arguments
fn args_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Vec<Expr>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    expr.separated_by(just(TokenKind::Comma)).collect()
}

/// Parser for struct field initializers: name: expr
fn field_init_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, FieldInit, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(expr)
        .map_with(|(name, value), e| FieldInit {
            name,
            value: Box::new(value),
            span: to_rue_span(e.span()),
        })
}

/// Parser for comma-separated field initializers
fn field_inits_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Vec<FieldInit>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    field_init_parser(expr)
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect()
}

/// Helper to create binary expression
fn make_binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    let span = Span::new(left.span().start, right.span().end);
    Expr::Binary(BinaryExpr {
        left: Box::new(left),
        op,
        right: Box::new(right),
        span,
    })
}

/// Helper to create unary expression
fn make_unary(op: UnaryOp, operand: Expr, op_span: SimpleSpan) -> Expr {
    let span = Span::new(op_span.start as u32, operand.span().end);
    Expr::Unary(UnaryExpr {
        op,
        operand: Box::new(operand),
        span,
    })
}

/// Expression parser with Pratt parsing for operator precedence
fn expr_parser<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(|expr| {
        // Atom parser - primary expressions
        let atom = atom_parser(expr.clone());

        // Build Pratt parser with precedence levels
        // Precedence (lower number = lower precedence, binds less tightly):
        // 1: || (logical or)
        // 2: && (logical and)
        // 3: ==, !=, <, >, <=, >= (comparison)
        // 4: +, - (additive)
        // 5: *, /, % (multiplicative)
        // 6: unary -, ! (prefix)
        atom.pratt((
            // Prefix operators (highest precedence for unary)
            prefix(
                6,
                just(TokenKind::Minus).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::Neg, rhs, op_span),
            ),
            prefix(
                6,
                just(TokenKind::Bang).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::Not, rhs, op_span),
            ),
            // Multiplicative (precedence 5)
            infix(left(5), just(TokenKind::Star), |l, _, r, _| {
                make_binary(l, BinaryOp::Mul, r)
            }),
            infix(left(5), just(TokenKind::Slash), |l, _, r, _| {
                make_binary(l, BinaryOp::Div, r)
            }),
            infix(left(5), just(TokenKind::Percent), |l, _, r, _| {
                make_binary(l, BinaryOp::Mod, r)
            }),
            // Additive (precedence 4)
            infix(left(4), just(TokenKind::Plus), |l, _, r, _| {
                make_binary(l, BinaryOp::Add, r)
            }),
            infix(left(4), just(TokenKind::Minus), |l, _, r, _| {
                make_binary(l, BinaryOp::Sub, r)
            }),
            // Comparison (precedence 3)
            infix(left(3), just(TokenKind::EqEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Eq, r)
            }),
            infix(left(3), just(TokenKind::BangEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Ne, r)
            }),
            infix(left(3), just(TokenKind::Lt), |l, _, r, _| {
                make_binary(l, BinaryOp::Lt, r)
            }),
            infix(left(3), just(TokenKind::Gt), |l, _, r, _| {
                make_binary(l, BinaryOp::Gt, r)
            }),
            infix(left(3), just(TokenKind::LtEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Le, r)
            }),
            infix(left(3), just(TokenKind::GtEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Ge, r)
            }),
            // Logical AND (precedence 2)
            infix(left(2), just(TokenKind::AmpAmp), |l, _, r, _| {
                make_binary(l, BinaryOp::And, r)
            }),
            // Logical OR (precedence 1, lowest)
            infix(left(1), just(TokenKind::PipePipe), |l, _, r, _| {
                make_binary(l, BinaryOp::Or, r)
            }),
        ))
    })
}

/// Atom parser - primary expressions (literals, identifiers, parens, blocks, control flow)
fn atom_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Integer literal
    let int_lit = select! {
        TokenKind::Int(n) = e => Expr::Int(IntLit {
            value: n,
            span: to_rue_span(e.span()),
        }),
    };

    // Boolean literals
    let bool_true = select! {
        TokenKind::True = e => Expr::Bool(BoolLit {
            value: true,
            span: to_rue_span(e.span()),
        }),
    };

    let bool_false = select! {
        TokenKind::False = e => Expr::Bool(BoolLit {
            value: false,
            span: to_rue_span(e.span()),
        }),
    };

    // Break
    let break_expr = select! {
        TokenKind::Break = e => Expr::Break(BreakExpr { span: to_rue_span(e.span()) }),
    };

    // Continue
    let continue_expr = select! {
        TokenKind::Continue = e => Expr::Continue(ContinueExpr { span: to_rue_span(e.span()) }),
    };

    // If expression
    let if_expr = just(TokenKind::If)
        .ignore_then(expr.clone())
        .then(maybe_unit_block_parser(expr.clone()))
        .then(
            just(TokenKind::Else)
                .ignore_then(maybe_unit_block_parser(expr.clone()))
                .or_not(),
        )
        .map_with(|((cond, then_block), else_block), e| {
            Expr::If(IfExpr {
                cond: Box::new(cond),
                then_block,
                else_block,
                span: to_rue_span(e.span()),
            })
        })
        .boxed();

    // While expression
    let while_expr = just(TokenKind::While)
        .ignore_then(expr.clone())
        .then(maybe_unit_block_parser(expr.clone()))
        .map_with(|(cond, body), e| {
            Expr::While(WhileExpr {
                cond: Box::new(cond),
                body,
                span: to_rue_span(e.span()),
            })
        })
        .boxed();

    // What can follow an identifier: call args, struct fields, or nothing
    #[derive(Clone)]
    enum IdentSuffix {
        Call(Vec<Expr>),
        StructLit(Vec<FieldInit>),
        None,
    }

    // Identifier followed by optional call args, struct literal, or nothing
    let ident_or_call_or_struct = ident_parser()
        .then(
            choice((
                // Function call: (args)
                args_parser(expr.clone())
                    .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
                    .map(IdentSuffix::Call),
                // Struct literal: { field: value, ... }
                field_inits_parser(expr.clone())
                    .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
                    .map(IdentSuffix::StructLit),
            ))
            .or_not()
            .map(|opt| opt.unwrap_or(IdentSuffix::None)),
        )
        .map_with(|(name, suffix), e| match suffix {
            IdentSuffix::Call(args) => Expr::Call(CallExpr {
                name,
                args,
                span: to_rue_span(e.span()),
            }),
            IdentSuffix::StructLit(fields) => Expr::StructLit(StructLitExpr {
                name,
                fields,
                span: to_rue_span(e.span()),
            }),
            IdentSuffix::None => Expr::Ident(name),
        });

    // Parenthesized expression
    let paren_expr = expr
        .clone()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .map_with(|inner, e| {
            Expr::Paren(ParenExpr {
                inner: Box::new(inner),
                span: to_rue_span(e.span()),
            })
        });

    // Block expression
    let block_expr = block_parser(expr.clone());

    // Intrinsic call: @name(args)
    let intrinsic_call = just(TokenKind::At)
        .ignore_then(ident_parser())
        .then(args_parser(expr).delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .map_with(|(name, args), e| {
            Expr::IntrinsicCall(IntrinsicCallExpr {
                name,
                args,
                span: to_rue_span(e.span()),
            })
        });

    // Primary expression (before field access)
    let primary = choice((
        int_lit,
        bool_true,
        bool_false,
        break_expr,
        continue_expr,
        if_expr,
        while_expr,
        intrinsic_call,
        ident_or_call_or_struct,
        paren_expr,
        block_expr,
    ));

    // Field access suffix: .field
    // Handles chains like a.b.c
    primary.foldl(
        just(TokenKind::Dot).ignore_then(ident_parser()).repeated(),
        |base, field| {
            let span = Span::new(base.span().start, field.span.end);
            Expr::Field(FieldExpr {
                base: Box::new(base),
                field,
                span,
            })
        },
    )
}

/// A block item is either a statement or an expression (potentially the final one)
#[derive(Debug, Clone)]
enum BlockItem {
    Statement(Statement),
    Expr(Expr),
}

/// Parser for let statements: let [mut] name [: type] = expr;
fn let_statement_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Statement, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::Let)
        .ignore_then(just(TokenKind::Mut).or_not().map(|m| m.is_some()))
        .then(ident_parser())
        .then(just(TokenKind::Colon).ignore_then(ident_parser()).or_not())
        .then_ignore(just(TokenKind::Eq))
        .then(expr)
        .then_ignore(just(TokenKind::Semi))
        .map_with(|(((is_mut, name), ty), init), e| {
            Statement::Let(LetStatement {
                is_mut,
                name,
                ty,
                init: Box::new(init),
                span: to_rue_span(e.span()),
            })
        })
}

/// Parser for assignment target: either a simple variable or field access chain
/// Parses: name or name.field or name.field.field...
fn assign_target_parser<'src, I>()
-> impl Parser<'src, I, AssignTarget, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then(
            just(TokenKind::Dot)
                .ignore_then(ident_parser())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map(|(base_ident, field_chain)| {
            if field_chain.is_empty() {
                // Simple variable: x
                AssignTarget::Var(base_ident)
            } else {
                // Field access chain: x.a.b.c
                // Build up the FieldExpr from left to right
                let mut base_expr = Expr::Ident(base_ident);
                for field in field_chain {
                    let span = Span::new(base_expr.span().start, field.span.end);
                    base_expr = Expr::Field(FieldExpr {
                        base: Box::new(base_expr),
                        field,
                        span,
                    });
                }
                // Extract the FieldExpr from the final expression
                if let Expr::Field(field_expr) = base_expr {
                    AssignTarget::Field(field_expr)
                } else {
                    unreachable!("We just built a Field expression")
                }
            }
        })
}

/// Parser for assignment statements: target = expr;
/// Supports both variable assignment (x = 5) and field assignment (point.x = 5)
fn assign_statement_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Statement, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    assign_target_parser()
        .then_ignore(just(TokenKind::Eq))
        .then(expr)
        .then_ignore(just(TokenKind::Semi))
        .map_with(|(target, value), e| {
            Statement::Assign(AssignStatement {
                target,
                value: Box::new(value),
                span: to_rue_span(e.span()),
            })
        })
}

/// Returns true if the expression can be used as a statement without a semicolon
fn is_control_flow_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(_) | Expr::While(_) | Expr::Break(_) | Expr::Continue(_)
    )
}

/// Parser for a single block item (statement or expression).
/// Parses: let statements, assignment statements, expression with semicolon,
/// or control flow without semicolon (but NOT followed by }).
fn block_item_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, BlockItem, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Let statement (always needs semicolon)
    let let_stmt = let_statement_parser(expr.clone()).map(BlockItem::Statement);

    // Assignment statement (always needs semicolon)
    let assign_stmt = assign_statement_parser(expr.clone()).map(BlockItem::Statement);

    // Expression with semicolon -> statement
    let expr_with_semi = expr
        .clone()
        .then_ignore(just(TokenKind::Semi))
        .map(|e| BlockItem::Statement(Statement::Expr(e)));

    // Control flow expression without semicolon, but NOT at end of block
    // We check that it's followed by something other than RBrace
    // This is a statement (no semicolon needed for control flow)
    let control_flow_stmt = expr
        .clone()
        .then_ignore(none_of([TokenKind::RBrace, TokenKind::Semi]).rewind())
        .try_map(|e, span| {
            if is_control_flow_expr(&e) {
                Ok(BlockItem::Statement(Statement::Expr(e)))
            } else {
                // Not control flow and no semicolon - this is an error
                // but let's try the final expr path
                Err(Rich::custom(span, "expected semicolon after expression"))
            }
        });

    // Expression at end of block (followed by }) -> final expression
    let final_expr = expr
        .then_ignore(just(TokenKind::RBrace).rewind())
        .map(BlockItem::Expr);

    choice((
        let_stmt,
        assign_stmt,
        expr_with_semi,
        control_flow_stmt,
        final_expr,
    ))
}

/// Process block items into statements and final expression
fn process_block_items(items: Vec<BlockItem>, block_span: Span) -> (Vec<Statement>, Expr) {
    let mut statements = Vec::new();
    let mut final_expr = None;

    for item in items {
        match item {
            BlockItem::Statement(stmt) => {
                if final_expr.is_some() {
                    // Had a non-semicolon expr before, but now we have more items
                    // This shouldn't happen with correct grammar, but handle gracefully
                    if let Some(e) = final_expr.take() {
                        statements.push(Statement::Expr(e));
                    }
                }
                statements.push(stmt);
            }
            BlockItem::Expr(e) => {
                if let Some(prev) = final_expr.take() {
                    // Had a non-semicolon expr before this one - that's invalid
                    // but we'll treat the previous as a statement for error recovery
                    statements.push(Statement::Expr(prev));
                }
                final_expr = Some(e);
            }
        }
    }

    let expr = final_expr.unwrap_or_else(|| {
        // No final expression - use a dummy false value (unit type placeholder)
        Expr::Bool(BoolLit {
            value: false,
            span: Span::new(block_span.end, block_span.end),
        })
    });

    (statements, expr)
}

/// Parser for blocks that may end without a final expression (for if/while bodies)
fn maybe_unit_block_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, BlockExpr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    block_item_parser(expr)
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
        .map_with(|items, e| {
            let span = to_rue_span(e.span());
            let (statements, final_expr) = process_block_items(items, span);
            BlockExpr {
                statements,
                expr: Box::new(final_expr),
                span,
            }
        })
}

/// Parser for blocks that require a final expression: { statements... expr }
fn block_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    block_item_parser(expr)
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
        .map_with(|items, e| {
            let span = to_rue_span(e.span());
            let (statements, final_expr) = process_block_items(items, span);
            Expr::Block(BlockExpr {
                statements,
                expr: Box::new(final_expr),
                span,
            })
        })
}

/// Parser for function definitions: fn name(params) -> Type { body }
fn function_parser<'src, I>()
-> impl Parser<'src, I, Function, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    just(TokenKind::Fn)
        .ignore_then(ident_parser())
        .then(params_parser().delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .then_ignore(just(TokenKind::Arrow))
        .then(ident_parser())
        .then(block_parser(expr))
        .map_with(|(((name, params), return_type), body), e| Function {
            name,
            params,
            return_type,
            body,
            span: to_rue_span(e.span()),
        })
}

/// Parser for struct definitions: struct Name { field: Type, ... }
fn struct_parser<'src, I>()
-> impl Parser<'src, I, StructDecl, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::Struct)
        .ignore_then(ident_parser())
        .then(field_decls_parser().delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(|(name, fields), e| StructDecl {
            name,
            fields,
            span: to_rue_span(e.span()),
        })
}

/// Parser for top-level items (functions and structs)
fn item_parser<'src, I>() -> impl Parser<'src, I, Item, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        function_parser().map(Item::Function),
        struct_parser().map(Item::Struct),
    ))
}

/// Main parser that produces an AST
fn ast_parser<'src, I>() -> impl Parser<'src, I, Ast, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    item_parser()
        .repeated()
        .collect::<Vec<_>>()
        .then_ignore(end())
        .map(|items| Ast { items })
}

/// Convert chumsky Rich error to CompileError
fn convert_error(err: Rich<'_, TokenKind>) -> CompileError {
    let span = to_rue_span(*err.span());

    match err.reason() {
        chumsky::error::RichReason::ExpectedFound { expected, found } => {
            let expected_str = if expected.is_empty() {
                "something".to_string()
            } else {
                expected
                    .iter()
                    .take(3) // Limit to first 3 for readability
                    .map(|e| format!("{:?}", e))
                    .collect::<Vec<_>>()
                    .join(" or ")
            };

            let found_str = found
                .as_ref()
                .map(|t| t.name())
                .unwrap_or("end of file")
                .to_string();

            CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: Box::leak(expected_str.into_boxed_str()),
                    found: found_str,
                },
                span,
            )
        }
        _ => CompileError::new(
            ErrorKind::UnexpectedToken {
                expected: "valid syntax",
                found: "parse error".to_string(),
            },
            span,
        ),
    }
}

/// Chumsky-based parser that converts tokens into an AST.
pub struct ChumskyParser {
    tokens: Vec<(TokenKind, SimpleSpan)>,
    source_len: usize,
}

impl ChumskyParser {
    /// Create a new parser from tokens produced by the lexer.
    pub fn new(tokens: Vec<rue_lexer::Token>) -> Self {
        let source_len = tokens.last().map(|t| t.span.end as usize).unwrap_or(0);

        let spanned_tokens: Vec<(TokenKind, SimpleSpan)> = tokens
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof) // Remove EOF, chumsky handles end differently
            .map(|t| {
                (
                    t.kind,
                    SimpleSpan::new(t.span.start as usize, t.span.end as usize),
                )
            })
            .collect();
        Self {
            tokens: spanned_tokens,
            source_len,
        }
    }

    /// Parse the tokens into an AST.
    pub fn parse(&self) -> CompileResult<Ast> {
        // Create a stream from the token iterator
        let token_iter = self.tokens.iter().cloned();
        let stream = Stream::from_iter(token_iter);

        // Map the stream to split (Token, Span) tuples
        let eoi: SimpleSpan = (self.source_len..self.source_len).into();
        let mapped = stream.map(eoi, |(tok, span)| (tok, span));

        ast_parser()
            .parse(mapped)
            .into_result()
            .map_err(|errs| convert_error(errs.into_iter().next().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn parse(source: &str) -> CompileResult<Ast> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize()?;
        let parser = ChumskyParser::new(tokens);
        parser.parse()
    }

    fn parse_expr(source: &str) -> CompileResult<Expr> {
        let ast = parse(&format!("fn main() -> i32 {{ {} }}", source))?;
        match ast.items.into_iter().next().unwrap() {
            Item::Function(f) => match f.body {
                Expr::Block(block) => Ok(*block.expr),
                other => Ok(other),
            },
            Item::Struct(_) => panic!("parse_expr helper should only be used with functions"),
        }
    }

    #[test]
    fn test_chumsky_parse_main() {
        let ast = parse("fn main() -> i32 { 42 }").unwrap();

        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.name.name, "main");
                assert_eq!(f.return_type.name, "i32");
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::Int(lit) => assert_eq!(lit.value, 42),
                        _ => panic!("expected Int"),
                    },
                    _ => panic!("expected Block"),
                }
            }
            Item::Struct(_) => panic!("expected Function"),
        }
    }

    #[test]
    fn test_chumsky_parse_addition() {
        let expr = parse_expr("1 + 2").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Add));
                match (*bin.left, *bin.right) {
                    (Expr::Int(l), Expr::Int(r)) => {
                        assert_eq!(l.value, 1);
                        assert_eq!(r.value, 2);
                    }
                    _ => panic!("expected Int operands"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_chumsky_parse_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let expr = parse_expr("1 + 2 * 3").unwrap();
        match expr {
            Expr::Binary(bin) => {
                assert!(matches!(bin.op, BinaryOp::Add));
                match *bin.left {
                    Expr::Int(l) => assert_eq!(l.value, 1),
                    _ => panic!("expected Int"),
                }
                match *bin.right {
                    Expr::Binary(inner) => {
                        assert!(matches!(inner.op, BinaryOp::Mul));
                    }
                    _ => panic!("expected Binary"),
                }
            }
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn test_chumsky_parse_let_binding() {
        let ast = parse("fn main() -> i32 { let x = 42; x }").unwrap();
        match &ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    assert_eq!(block.statements.len(), 1);
                    match &block.statements[0] {
                        Statement::Let(let_stmt) => {
                            assert!(!let_stmt.is_mut);
                            assert_eq!(let_stmt.name.name, "x");
                        }
                        _ => panic!("expected Let"),
                    }
                }
                _ => panic!("expected Block"),
            },
            Item::Struct(_) => panic!("expected Function"),
        }
    }

    #[test]
    fn test_while_simple() {
        // Simplest while case
        let ast = parse("fn main() -> i32 { while true { } 0 }").unwrap();
        assert_eq!(ast.items.len(), 1);
    }

    #[test]
    fn test_while_with_statement() {
        // While with assignment
        let ast = parse("fn main() -> i32 { while true { x = 1; } 0 }").unwrap();
        assert_eq!(ast.items.len(), 1);
    }

    #[test]
    fn test_function_calls() {
        let ast = parse("fn add(a: i32, b: i32) -> i32 { a + b } fn main() -> i32 { add(1, 2) }")
            .unwrap();
        assert_eq!(ast.items.len(), 2);
    }

    #[test]
    fn test_if_else() {
        let ast = parse("fn main() -> i32 { if true { 1 } else { 0 } }").unwrap();
        assert_eq!(ast.items.len(), 1);
    }

    #[test]
    fn test_nested_control_flow() {
        let ast =
            parse("fn main() -> i32 { let mut x = 0; while x < 10 { x = x + 1; } x }").unwrap();
        assert_eq!(ast.items.len(), 1);
    }
}
