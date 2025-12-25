//! Chumsky-based parser for the Rue programming language.
//!
//! This module provides a parser implementation using chumsky combinators
//! with Pratt parsing for expression precedence.

use crate::ast::{
    ArrayLitExpr, AssignStatement, AssignTarget, AssocFnCallExpr, Ast, BinaryExpr, BinaryOp,
    BlockExpr, BoolLit, BreakExpr, CallExpr, ContinueExpr, Directive, DirectiveArg, EnumDecl,
    EnumVariant, Expr, FieldDecl, FieldExpr, FieldInit, Function, Ident, IfExpr, ImplBlock,
    IndexExpr, IntLit, IntrinsicArg, IntrinsicCallExpr, Item, LetPattern, LetStatement, LoopExpr,
    MatchArm, MatchExpr, Method, MethodCallExpr, NegIntLit, Param, ParenExpr, PathExpr,
    PathPattern, Pattern, ReturnExpr, SelfExpr, SelfParam, Statement, StringLit, StructDecl,
    StructLitExpr, TypeExpr, UnaryExpr, UnaryOp, UnitLit, WhileExpr,
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

/// Parser for primitive type keywords: i8, i16, i32, i64, u8, u16, u32, u64, bool
/// These are reserved keywords that cannot be used as identifiers.
fn primitive_type_parser<'src, I>()
-> impl Parser<'src, I, TypeExpr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    select! {
        TokenKind::I8 = e => TypeExpr::Named(Ident { name: "i8".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::I16 = e => TypeExpr::Named(Ident { name: "i16".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::I32 = e => TypeExpr::Named(Ident { name: "i32".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::I64 = e => TypeExpr::Named(Ident { name: "i64".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::U8 = e => TypeExpr::Named(Ident { name: "u8".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::U16 = e => TypeExpr::Named(Ident { name: "u16".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::U32 = e => TypeExpr::Named(Ident { name: "u32".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::U64 = e => TypeExpr::Named(Ident { name: "u64".to_string(), span: to_rue_span(e.span()) }),
        TokenKind::Bool = e => TypeExpr::Named(Ident { name: "bool".to_string(), span: to_rue_span(e.span()) }),
    }
}

/// Parser for type expressions: primitive types (i32, bool, etc.), () for unit, ! for never, or [T; N] for arrays
fn type_parser<'src, I>()
-> impl Parser<'src, I, TypeExpr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(|ty| {
        // Unit type: ()
        let unit_type = just(TokenKind::LParen)
            .then(just(TokenKind::RParen))
            .map_with(|_, e| TypeExpr::Unit(to_rue_span(e.span())));

        // Never type: !
        let never_type =
            just(TokenKind::Bang).map_with(|_, e| TypeExpr::Never(to_rue_span(e.span())));

        // Array type: [T; N]
        let array_type = just(TokenKind::LBracket)
            .ignore_then(ty)
            .then_ignore(just(TokenKind::Semi))
            .then(select! {
                TokenKind::Int(n) => n as u64,
            })
            .then_ignore(just(TokenKind::RBracket))
            .map_with(|(element, length), e| TypeExpr::Array {
                element: Box::new(element),
                length,
                span: to_rue_span(e.span()),
            });

        // Named type: user-defined types like MyStruct
        let named_type = ident_parser().map(TypeExpr::Named);

        choice((
            unit_type,
            never_type,
            array_type,
            primitive_type_parser(),
            named_type,
        ))
    })
}

/// Parser for function parameters: name: type
fn param_parser<'src, I>() -> impl Parser<'src, I, Param, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
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
        .then(type_parser())
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

/// Parser for a single directive: @name(arg1, arg2, ...)
fn directive_parser<'src, I>()
-> impl Parser<'src, I, Directive, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::At)
        .ignore_then(ident_parser())
        .then(
            ident_parser()
                .map(DirectiveArg::Ident)
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|(name, args), e| Directive {
            name,
            args,
            span: to_rue_span(e.span()),
        })
}

/// Parser for zero or more directives
fn directives_parser<'src, I>()
-> impl Parser<'src, I, Vec<Directive>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    directive_parser().repeated().collect()
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
        // 3: | (bitwise or)
        // 4: ^ (bitwise xor)
        // 5: & (bitwise and)
        // 6: ==, !=, <, >, <=, >= (comparison)
        // 7: <<, >> (shift)
        // 8: +, - (additive)
        // 9: *, /, % (multiplicative)
        // 10: unary -, !, ~ (prefix)
        atom.pratt((
            // Prefix operators (highest precedence for unary)
            prefix(
                10,
                just(TokenKind::Minus).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::Neg, rhs, op_span),
            ),
            prefix(
                10,
                just(TokenKind::Bang).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::Not, rhs, op_span),
            ),
            prefix(
                10,
                just(TokenKind::Tilde).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::BitNot, rhs, op_span),
            ),
            // Multiplicative (precedence 9)
            infix(left(9), just(TokenKind::Star), |l, _, r, _| {
                make_binary(l, BinaryOp::Mul, r)
            }),
            infix(left(9), just(TokenKind::Slash), |l, _, r, _| {
                make_binary(l, BinaryOp::Div, r)
            }),
            infix(left(9), just(TokenKind::Percent), |l, _, r, _| {
                make_binary(l, BinaryOp::Mod, r)
            }),
            // Additive (precedence 8)
            infix(left(8), just(TokenKind::Plus), |l, _, r, _| {
                make_binary(l, BinaryOp::Add, r)
            }),
            infix(left(8), just(TokenKind::Minus), |l, _, r, _| {
                make_binary(l, BinaryOp::Sub, r)
            }),
            // Shift (precedence 7)
            infix(left(7), just(TokenKind::LtLt), |l, _, r, _| {
                make_binary(l, BinaryOp::Shl, r)
            }),
            infix(left(7), just(TokenKind::GtGt), |l, _, r, _| {
                make_binary(l, BinaryOp::Shr, r)
            }),
            // Comparison (precedence 6)
            infix(left(6), just(TokenKind::EqEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Eq, r)
            }),
            infix(left(6), just(TokenKind::BangEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Ne, r)
            }),
            infix(left(6), just(TokenKind::Lt), |l, _, r, _| {
                make_binary(l, BinaryOp::Lt, r)
            }),
            infix(left(6), just(TokenKind::Gt), |l, _, r, _| {
                make_binary(l, BinaryOp::Gt, r)
            }),
            infix(left(6), just(TokenKind::LtEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Le, r)
            }),
            infix(left(6), just(TokenKind::GtEq), |l, _, r, _| {
                make_binary(l, BinaryOp::Ge, r)
            }),
            // Bitwise AND (precedence 5)
            infix(left(5), just(TokenKind::Amp), |l, _, r, _| {
                make_binary(l, BinaryOp::BitAnd, r)
            }),
            // Bitwise XOR (precedence 4)
            infix(left(4), just(TokenKind::Caret), |l, _, r, _| {
                make_binary(l, BinaryOp::BitXor, r)
            }),
            // Bitwise OR (precedence 3)
            infix(left(3), just(TokenKind::Pipe), |l, _, r, _| {
                make_binary(l, BinaryOp::BitOr, r)
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

/// Parser for patterns in match arms
fn pattern_parser<'src, I>()
-> impl Parser<'src, I, Pattern, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Wildcard pattern: _
    let wildcard =
        just(TokenKind::Underscore).map_with(|_, e| Pattern::Wildcard(to_rue_span(e.span())));

    // Integer literal pattern (positive or zero)
    let int_pat = select! {
        TokenKind::Int(n) = e => Pattern::Int(IntLit {
            value: n,
            span: to_rue_span(e.span()),
        }),
    };

    // Negative integer literal pattern: - followed by integer
    let neg_int_pat = just(TokenKind::Minus)
        .then(select! { TokenKind::Int(n) => n })
        .map_with(|(_, n), e| {
            Pattern::NegInt(NegIntLit {
                value: n,
                span: to_rue_span(e.span()),
            })
        });

    // Boolean literal patterns
    let bool_true = select! {
        TokenKind::True = e => Pattern::Bool(BoolLit {
            value: true,
            span: to_rue_span(e.span()),
        }),
    };

    let bool_false = select! {
        TokenKind::False = e => Pattern::Bool(BoolLit {
            value: false,
            span: to_rue_span(e.span()),
        }),
    };

    // Path pattern: Enum::Variant
    let path_pat = ident_parser()
        .then_ignore(just(TokenKind::ColonColon))
        .then(ident_parser())
        .map_with(|(type_name, variant), e| {
            Pattern::Path(PathPattern {
                type_name,
                variant,
                span: to_rue_span(e.span()),
            })
        });

    choice((
        wildcard,
        neg_int_pat,
        int_pat,
        bool_true,
        bool_false,
        path_pat,
    ))
}

/// Parser for a single match arm: pattern => expr
fn match_arm_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, MatchArm, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    pattern_parser()
        .then_ignore(just(TokenKind::FatArrow))
        .then(expr)
        .map_with(|(pattern, body), e| MatchArm {
            pattern,
            body: Box::new(body),
            span: to_rue_span(e.span()),
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

    // String literal
    let string_lit = select! {
        TokenKind::String(s) = e => Expr::String(StringLit {
            value: s,
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

    // Unit literal: ()
    let unit_lit = just(TokenKind::LParen)
        .then(just(TokenKind::RParen))
        .map_with(|_, e| {
            Expr::Unit(UnitLit {
                span: to_rue_span(e.span()),
            })
        });

    // Break
    let break_expr = select! {
        TokenKind::Break = e => Expr::Break(BreakExpr { span: to_rue_span(e.span()) }),
    };

    // Continue
    let continue_expr = select! {
        TokenKind::Continue = e => Expr::Continue(ContinueExpr { span: to_rue_span(e.span()) }),
    };

    // Self expression (in method bodies)
    let self_expr = select! {
        TokenKind::SelfValue = e => Expr::SelfExpr(SelfExpr { span: to_rue_span(e.span()) }),
    };

    // Return expression: return <expr>? (expression is optional for unit-returning functions)
    let return_expr = just(TokenKind::Return)
        .ignore_then(expr.clone().or_not())
        .map_with(|value, e| {
            Expr::Return(ReturnExpr {
                value: value.map(Box::new),
                span: to_rue_span(e.span()),
            })
        });

    // If expression - defined with recursive reference to allow `else if` chains
    let if_expr = recursive(|if_expr_rec| {
        just(TokenKind::If)
            .ignore_then(expr.clone())
            .then(maybe_unit_block_parser(expr.clone()))
            .then(
                just(TokenKind::Else)
                    .ignore_then(choice((
                        // else if: wrap the nested if in a synthetic block
                        if_expr_rec.map_with(|nested_if, e| {
                            let span = to_rue_span(e.span());
                            BlockExpr {
                                statements: Vec::new(),
                                expr: Box::new(nested_if),
                                span,
                            }
                        }),
                        // else { ... }: parse a regular block
                        maybe_unit_block_parser(expr.clone()),
                    )))
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

    // Loop expression (infinite loop)
    let loop_expr = just(TokenKind::Loop)
        .ignore_then(maybe_unit_block_parser(expr.clone()))
        .map_with(|body, e| {
            Expr::Loop(LoopExpr {
                body,
                span: to_rue_span(e.span()),
            })
        })
        .boxed();

    // Match expression: match scrutinee { pattern => expr, ... }
    let match_expr = just(TokenKind::Match)
        .ignore_then(expr.clone())
        .then(
            match_arm_parser(expr.clone())
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(scrutinee, arms), e| {
            Expr::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms,
                span: to_rue_span(e.span()),
            })
        })
        .boxed();

    // What can follow an identifier: call args, struct fields, path (::variant), path call (::fn()), or nothing
    #[derive(Clone)]
    enum IdentSuffix {
        Call(Vec<Expr>),
        StructLit(Vec<FieldInit>),
        Path(Ident),                // ::Variant (for enum variants)
        PathCall(Ident, Vec<Expr>), // ::function() (for associated functions)
        None,
    }

    // Identifier followed by optional call args, struct literal, path, or nothing
    let ident_or_call_or_struct_or_path = ident_parser()
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
                // Associated function call: ::function(args)
                just(TokenKind::ColonColon)
                    .ignore_then(ident_parser())
                    .then(
                        args_parser(expr.clone())
                            .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
                    )
                    .map(|(func, args)| IdentSuffix::PathCall(func, args)),
                // Path: ::Variant (for enum variants)
                just(TokenKind::ColonColon)
                    .ignore_then(ident_parser())
                    .map(IdentSuffix::Path),
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
            IdentSuffix::PathCall(function, args) => Expr::AssocFnCall(AssocFnCallExpr {
                type_name: name,
                function,
                args,
                span: to_rue_span(e.span()),
            }),
            IdentSuffix::Path(variant) => Expr::Path(PathExpr {
                type_name: name,
                variant,
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

    // Intrinsic argument: can be either a type or an expression
    // We parse as type only for unambiguous type syntax (primitives, (), !, [T;N])
    // Bare identifiers are parsed as expressions since they could be variables
    let unambiguous_type = {
        // Unit type: ()
        let unit_type = just(TokenKind::LParen)
            .then(just(TokenKind::RParen))
            .map_with(|_, e| IntrinsicArg::Type(TypeExpr::Unit(to_rue_span(e.span()))));

        // Never type: !
        let never_type = just(TokenKind::Bang)
            .map_with(|_, e| IntrinsicArg::Type(TypeExpr::Never(to_rue_span(e.span()))));

        // Array type: [T; N]
        let array_type = just(TokenKind::LBracket)
            .ignore_then(type_parser())
            .then_ignore(just(TokenKind::Semi))
            .then(select! {
                TokenKind::Int(n) => n as u64,
            })
            .then_ignore(just(TokenKind::RBracket))
            .map_with(|(element, length), e| {
                IntrinsicArg::Type(TypeExpr::Array {
                    element: Box::new(element),
                    length,
                    span: to_rue_span(e.span()),
                })
            });

        // Primitive type keywords (these can't be variable names)
        let primitive_type = primitive_type_parser().map(IntrinsicArg::Type);

        choice((unit_type, never_type, array_type, primitive_type))
    };

    // Try unambiguous type syntax first, then fall back to expression
    let intrinsic_arg = unambiguous_type.or(expr.clone().map(IntrinsicArg::Expr));

    // Intrinsic call: @name(args)
    let intrinsic_call = just(TokenKind::At)
        .ignore_then(ident_parser())
        .then(
            intrinsic_arg
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|(name, args), e| {
            Expr::IntrinsicCall(IntrinsicCallExpr {
                name,
                args,
                span: to_rue_span(e.span()),
            })
        });

    // Array literal: [expr, expr, ...]
    let array_lit = args_parser(expr.clone())
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|elements, e| {
            Expr::ArrayLit(ArrayLitExpr {
                elements,
                span: to_rue_span(e.span()),
            })
        });

    // Primary expression (before field access and indexing)
    // Note: unit_lit must come before paren_expr so () is parsed as unit, not empty parens
    // Note: self_expr must come before ident_or_call_or_struct_or_path since self is a keyword
    let primary = choice((
        int_lit,
        string_lit,
        bool_true,
        bool_false,
        unit_lit,
        break_expr,
        continue_expr,
        self_expr,
        return_expr,
        if_expr,
        match_expr,
        while_expr,
        loop_expr,
        intrinsic_call,
        array_lit,
        ident_or_call_or_struct_or_path,
        paren_expr,
        block_expr,
    ));

    // Suffix for field access (.field), method call (.method(args)), or indexing ([expr])
    #[derive(Clone)]
    enum Suffix {
        Field(Ident),
        MethodCall(Ident, Vec<Expr>),
        Index(Expr),
    }

    // Method call: .ident(args)
    let method_call_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then(
            args_parser(expr.clone())
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map(|(method, args)| Suffix::MethodCall(method, args));

    // Field access: .ident (but NOT followed by ()
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then_ignore(none_of([TokenKind::LParen]).rewind())
        .map(Suffix::Field);

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map(Suffix::Index);

    // Field access, method call, and indexing suffix: .field, .method(), or [expr]
    // Method call must come before field access to catch .method(args) before .field
    // Handles chains like a.b.c or a[0][1] or a[0].field or a.method().field
    primary.foldl(
        choice((method_call_suffix, field_suffix, index_suffix)).repeated(),
        |base, suffix| match suffix {
            Suffix::Field(field) => {
                let span = Span::new(base.span().start, field.span.end);
                Expr::Field(FieldExpr {
                    base: Box::new(base),
                    field,
                    span,
                })
            }
            Suffix::MethodCall(method, args) => {
                let end = if args.is_empty() {
                    // Span ends after the closing paren, but we don't have that span
                    // We'll use the method name span end + 2 for "()" as approximation
                    method.span.end + 2
                } else {
                    args.last().unwrap().span().end + 1
                };
                let span = Span::new(base.span().start, end);
                Expr::MethodCall(MethodCallExpr {
                    receiver: Box::new(base),
                    method,
                    args,
                    span,
                })
            }
            Suffix::Index(index) => {
                let span = Span::new(base.span().start, index.span().end);
                Expr::Index(IndexExpr {
                    base: Box::new(base),
                    index: Box::new(index),
                    span,
                })
            }
        },
    )
}

/// A block item is either a statement or an expression (potentially the final one)
#[derive(Debug, Clone)]
enum BlockItem {
    Statement(Statement),
    Expr(Expr),
}

/// Parser for a let binding pattern: either an identifier or _ (wildcard/discard)
fn let_pattern_parser<'src, I>()
-> impl Parser<'src, I, LetPattern, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let wildcard =
        just(TokenKind::Underscore).map_with(|_, e| LetPattern::Wildcard(to_rue_span(e.span())));
    let ident = ident_parser().map(LetPattern::Ident);
    ident.or(wildcard)
}

/// Parser for let statements: [@directive]* let [mut] pattern [: type] = expr;
fn let_statement_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Statement, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    directives_parser()
        .then(just(TokenKind::Let).ignore_then(just(TokenKind::Mut).or_not().map(|m| m.is_some())))
        .then(let_pattern_parser())
        .then(just(TokenKind::Colon).ignore_then(type_parser()).or_not())
        .then_ignore(just(TokenKind::Eq))
        .then(expr)
        .then_ignore(just(TokenKind::Semi))
        .map_with(|((((directives, is_mut), pattern), ty), init), e| {
            Statement::Let(LetStatement {
                directives,
                is_mut,
                pattern,
                ty,
                init: Box::new(init),
                span: to_rue_span(e.span()),
            })
        })
}

/// Suffix for assignment targets: either .field or [index]
#[derive(Clone)]
enum AssignSuffix {
    Field(Ident),
    Index(Expr),
}

/// Parser for assignment target: variable, field access, or index access
/// Parses: name or name.field or name[idx] or name.field[idx].field...
fn assign_target_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, AssignTarget, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .map(AssignSuffix::Field);

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map(AssignSuffix::Index);

    ident_parser()
        .then(
            choice((field_suffix, index_suffix))
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map(|(base_ident, suffixes)| {
            if suffixes.is_empty() {
                // Simple variable: x
                AssignTarget::Var(base_ident)
            } else {
                // Chain of field/index accesses: x.a[0].b...
                // Build up the expression from left to right
                let mut base_expr = Expr::Ident(base_ident);
                for suffix in suffixes.iter().take(suffixes.len().saturating_sub(1)) {
                    match suffix {
                        AssignSuffix::Field(field) => {
                            let span = Span::new(base_expr.span().start, field.span.end);
                            base_expr = Expr::Field(FieldExpr {
                                base: Box::new(base_expr),
                                field: field.clone(),
                                span,
                            });
                        }
                        AssignSuffix::Index(index) => {
                            let span = Span::new(base_expr.span().start, index.span().end);
                            base_expr = Expr::Index(IndexExpr {
                                base: Box::new(base_expr),
                                index: Box::new(index.clone()),
                                span,
                            });
                        }
                    }
                }
                // The last suffix determines the target type
                match suffixes.last().unwrap() {
                    AssignSuffix::Field(field) => {
                        let span = Span::new(base_expr.span().start, field.span.end);
                        AssignTarget::Field(FieldExpr {
                            base: Box::new(base_expr),
                            field: field.clone(),
                            span,
                        })
                    }
                    AssignSuffix::Index(index) => {
                        let span = Span::new(base_expr.span().start, index.span().end);
                        AssignTarget::Index(IndexExpr {
                            base: Box::new(base_expr),
                            index: Box::new(index.clone()),
                            span,
                        })
                    }
                }
            }
        })
}

/// Parser for assignment statements: target = expr;
/// Supports variable (x = 5), field (point.x = 5), and index (arr[0] = 5) assignment
fn assign_statement_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Statement, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    assign_target_parser(expr.clone())
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

/// Returns true if the expression is a control flow construct that can appear
/// as a statement without a trailing semicolon (if, while, match, break, continue, return).
fn is_control_flow_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::Loop(_)
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::Return(_)
    )
}

/// Returns true if the expression diverges (has the Never type).
/// These expressions can be promoted to the final expression of a block
/// since Never coerces to any type.
fn is_diverging_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Break(_) | Expr::Continue(_) | Expr::Return(_) | Expr::Loop(_)
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
        // No explicit final expression. Check if the last statement is a diverging
        // expression (break, continue, return) - if so, promote it to the final
        // expression since it has type Never which coerces to any type.
        if let Some(Statement::Expr(e)) = statements.last() {
            if is_diverging_expr(e) {
                // Safe to unwrap: we just checked last() is Some(Statement::Expr(_))
                let Statement::Expr(e) = statements.pop().unwrap() else {
                    unreachable!()
                };
                return e;
            }
        }
        // Fallback: use a unit expression (block produces unit type)
        Expr::Unit(UnitLit {
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

/// Parser for function definitions: [@directive]* fn name(params) -> Type { body }
fn function_parser<'src, I>()
-> impl Parser<'src, I, Function, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    directives_parser()
        .then(just(TokenKind::Fn).ignore_then(ident_parser()))
        .then(params_parser().delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .then(just(TokenKind::Arrow).ignore_then(type_parser()).or_not())
        .then(block_parser(expr))
        .map_with(
            |((((directives, name), params), return_type), body), e| Function {
                directives,
                name,
                params,
                return_type,
                body,
                span: to_rue_span(e.span()),
            },
        )
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

/// Parser for enum variant: just an identifier
fn enum_variant_parser<'src, I>()
-> impl Parser<'src, I, EnumVariant, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser().map_with(|name, e| EnumVariant {
        name,
        span: to_rue_span(e.span()),
    })
}

/// Parser for comma-separated enum variants
fn enum_variants_parser<'src, I>()
-> impl Parser<'src, I, Vec<EnumVariant>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    enum_variant_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect()
}

/// Parser for enum definitions: enum Name { Variant1, Variant2, ... }
fn enum_parser<'src, I>()
-> impl Parser<'src, I, EnumDecl, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::Enum)
        .ignore_then(ident_parser())
        .then(enum_variants_parser().delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(|(name, variants), e| EnumDecl {
            name,
            variants,
            span: to_rue_span(e.span()),
        })
}

/// Parser for method definitions: [@directive]* fn name(self, params) -> Type { body }
/// Methods differ from functions in that they can have `self` as the first parameter.
fn method_parser<'src, I>()
-> impl Parser<'src, I, Method, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse optional self parameter
    let self_param = just(TokenKind::SelfValue).map_with(|_, e| SelfParam {
        span: to_rue_span(e.span()),
    });

    // Parse self followed by optional regular params
    let self_then_params = self_param
        .then(
            just(TokenKind::Comma)
                .ignore_then(params_parser())
                .or_not()
                .map(|opt| opt.unwrap_or_default()),
        )
        .map(|(self_param, params)| (Some(self_param), params));

    // Parse just regular params (no self) - this is an associated function
    let just_params = params_parser().map(|params| (None, params));

    // Try self first, then fall back to regular params
    let params_with_optional_self = self_then_params.or(just_params);

    directives_parser()
        .then(just(TokenKind::Fn).ignore_then(ident_parser()))
        .then(
            params_with_optional_self
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .then(just(TokenKind::Arrow).ignore_then(type_parser()).or_not())
        .then(block_parser(expr))
        .map_with(
            |((((directives, name), (receiver, params)), return_type), body), e| Method {
                directives,
                name,
                receiver,
                params,
                return_type,
                body,
                span: to_rue_span(e.span()),
            },
        )
}

/// Parser for impl blocks: impl Type { fn... }
fn impl_parser<'src, I>()
-> impl Parser<'src, I, ImplBlock, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    just(TokenKind::Impl)
        .ignore_then(ident_parser())
        .then(
            method_parser()
                .repeated()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(type_name, methods), e| ImplBlock {
            type_name,
            methods,
            span: to_rue_span(e.span()),
        })
}

/// Parser for top-level items (functions, structs, enums, and impl blocks)
fn item_parser<'src, I>() -> impl Parser<'src, I, Item, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        function_parser().map(Item::Function),
        struct_parser().map(Item::Struct),
        enum_parser().map(Item::Enum),
        impl_parser().map(Item::Impl),
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
            Item::Enum(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Impl(_) => panic!("parse_expr helper should only be used with functions"),
        }
    }

    #[test]
    fn test_chumsky_parse_main() {
        let ast = parse("fn main() -> i32 { 42 }").unwrap();

        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.name.name, "main");
                match f.return_type.as_ref().unwrap() {
                    TypeExpr::Named(ident) => assert_eq!(ident.name, "i32"),
                    _ => panic!("expected Named type"),
                }
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::Int(lit) => assert_eq!(lit.value, 42),
                        _ => panic!("expected Int"),
                    },
                    _ => panic!("expected Block"),
                }
            }
            Item::Struct(_) => panic!("expected Function"),
            Item::Enum(_) => panic!("expected Function"),
            Item::Impl(_) => panic!("expected Function"),
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
                            match &let_stmt.pattern {
                                LetPattern::Ident(ident) => assert_eq!(ident.name, "x"),
                                LetPattern::Wildcard(_) => panic!("expected Ident, got Wildcard"),
                            }
                        }
                        _ => panic!("expected Let"),
                    }
                }
                _ => panic!("expected Block"),
            },
            Item::Struct(_) => panic!("expected Function"),
            Item::Enum(_) => panic!("expected Function"),
            Item::Impl(_) => panic!("expected Function"),
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
