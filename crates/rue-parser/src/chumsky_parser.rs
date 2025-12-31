//! Chumsky-based parser for the Rue programming language.
//!
//! This module provides a parser implementation using chumsky combinators
//! with Pratt parsing for expression precedence.

use crate::ast::{
    ArgMode, ArrayLitExpr, AssignStatement, AssignTarget, AssocFnCallExpr, Ast, BinaryExpr,
    BinaryOp, BlockExpr, BoolLit, BreakExpr, CallArg, CallExpr, ContinueExpr, Directive,
    DirectiveArg, DropFn, EnumDecl, EnumVariant, Expr, FieldDecl, FieldExpr, FieldInit, Function,
    Ident, IfExpr, ImplBlock, IndexExpr, IntLit, IntrinsicArg, IntrinsicCallExpr, Item, LetPattern,
    LetStatement, LoopExpr, MatchArm, MatchExpr, Method, MethodCallExpr, NegIntLit, Param,
    ParamMode, ParenExpr, PathExpr, PathPattern, Pattern, ReturnExpr, SelfExpr, SelfParam,
    Statement, StringLit, StructDecl, StructLitExpr, TypeExpr, UnaryExpr, UnaryOp, UnitLit,
    WhileExpr,
};
use chumsky::input::{Input as ChumskyInput, Stream, ValueInput};
use chumsky::pratt::{infix, left, prefix};
use chumsky::prelude::*;
use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, ErrorKind, MultiErrorResult};
use rue_lexer::TokenKind;
use rue_span::Span;
use std::borrow::Cow;

use std::cell::RefCell;

/// Pre-interned symbols for primitive type names.
/// These are interned once when the parser is created and reused for all parsing.
#[derive(Clone, Copy)]
pub struct PrimitiveTypeSpurs {
    pub i8: Spur,
    pub i16: Spur,
    pub i32: Spur,
    pub i64: Spur,
    pub u8: Spur,
    pub u16: Spur,
    pub u32: Spur,
    pub u64: Spur,
    pub bool: Spur,
}

impl PrimitiveTypeSpurs {
    /// Create a new set of primitive type symbols by interning them.
    pub fn new(interner: &mut ThreadedRodeo) -> Self {
        Self {
            i8: interner.get_or_intern("i8"),
            i16: interner.get_or_intern("i16"),
            i32: interner.get_or_intern("i32"),
            i64: interner.get_or_intern("i64"),
            u8: interner.get_or_intern("u8"),
            u16: interner.get_or_intern("u16"),
            u32: interner.get_or_intern("u32"),
            u64: interner.get_or_intern("u64"),
            bool: interner.get_or_intern("bool"),
        }
    }
}

// Thread-local storage for the primitive type symbols during parsing.
// This is set before parsing and read during parsing.
thread_local! {
    static PRIMITIVE_SYMS: RefCell<Option<PrimitiveTypeSpurs>> = const { RefCell::new(None) };
}

/// Get the primitive type symbols. Panics if not set.
fn get_primitive_syms() -> PrimitiveTypeSpurs {
    PRIMITIVE_SYMS.with(|syms| {
        syms.borrow()
            .expect("Primitive type symbols not initialized - call parse() instead of using parsers directly")
    })
}

/// Convert a `usize` offset to `u32`, asserting it fits in debug builds.
///
/// # Panics
///
/// In debug builds, panics if `offset` exceeds `u32::MAX`.
/// This would only happen for source files larger than 4GB.
#[inline]
fn offset_to_u32(offset: usize) -> u32 {
    debug_assert!(
        offset <= u32::MAX as usize,
        "offset {} exceeds u32::MAX (source file too large)",
        offset
    );
    offset as u32
}

/// Convert chumsky SimpleSpan to rue_span::Span.
///
/// # Panics
///
/// In debug builds, panics if `span.start` or `span.end` exceeds `u32::MAX`.
/// This would only happen for source files larger than 4GB.
fn to_rue_span(span: SimpleSpan) -> Span {
    Span::new(offset_to_u32(span.start), offset_to_u32(span.end))
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
    let syms = get_primitive_syms();

    // Create individual parsers for each primitive type
    let i8_parser = just(TokenKind::I8).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.i8,
            span: to_rue_span(e.span()),
        })
    });
    let i16_parser = just(TokenKind::I16).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.i16,
            span: to_rue_span(e.span()),
        })
    });
    let i32_parser = just(TokenKind::I32).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.i32,
            span: to_rue_span(e.span()),
        })
    });
    let i64_parser = just(TokenKind::I64).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.i64,
            span: to_rue_span(e.span()),
        })
    });
    let u8_parser = just(TokenKind::U8).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.u8,
            span: to_rue_span(e.span()),
        })
    });
    let u16_parser = just(TokenKind::U16).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.u16,
            span: to_rue_span(e.span()),
        })
    });
    let u32_parser = just(TokenKind::U32).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.u32,
            span: to_rue_span(e.span()),
        })
    });
    let u64_parser = just(TokenKind::U64).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.u64,
            span: to_rue_span(e.span()),
        })
    });
    let bool_parser = just(TokenKind::Bool).map_with(move |_, e| {
        TypeExpr::Named(Ident {
            name: syms.bool,
            span: to_rue_span(e.span()),
        })
    });

    choice((
        i8_parser,
        i16_parser,
        i32_parser,
        i64_parser,
        u8_parser,
        u16_parser,
        u32_parser,
        u64_parser,
        bool_parser,
    ))
}

/// Parser for type expressions: primitive types (i32, bool, etc.), () for unit, ! for never, or [T; N] for arrays
fn type_parser<'src, I>()
-> impl Parser<'src, I, TypeExpr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(move |ty| {
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

/// Parser for parameter mode: inout or borrow
fn param_mode_parser<'src, I>()
-> impl Parser<'src, I, ParamMode, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        just(TokenKind::Inout).to(ParamMode::Inout),
        just(TokenKind::Borrow).to(ParamMode::Borrow),
    ))
}

/// Parser for function parameters: [inout|borrow] name: type
fn param_parser<'src, I>() -> impl Parser<'src, I, Param, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    param_mode_parser()
        .or_not()
        .then(ident_parser())
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|((mode, name), ty), e| Param {
            mode: mode.unwrap_or(ParamMode::Normal),
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

/// Parser for a single directive: @name or @name(arg1, arg2, ...)
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
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
                .or_not(),
        )
        .map_with(|(name, args), e| Directive {
            name,
            args: args.unwrap_or_default(),
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

/// Parser for argument mode: inout or borrow
fn arg_mode_parser<'src, I>()
-> impl Parser<'src, I, ArgMode, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        just(TokenKind::Inout).to(ArgMode::Inout),
        just(TokenKind::Borrow).to(ArgMode::Borrow),
    ))
}

/// Parser for a single call argument: [inout|borrow] expr
fn call_arg_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, CallArg, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    arg_mode_parser()
        .or_not()
        .then(expr)
        .map_with(|(mode, expr), e| CallArg {
            mode: mode.unwrap_or(ArgMode::Normal),
            expr,
            span: to_rue_span(e.span()),
        })
}

/// Parser for comma-separated call arguments with optional inout
fn call_args_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Vec<CallArg>, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    call_arg_parser(expr)
        .separated_by(just(TokenKind::Comma))
        .collect()
}

/// Parser for comma-separated expression arguments (for contexts that don't support inout)
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
    let span = Span::new(offset_to_u32(op_span.start), operand.span().end);
    Expr::Unary(UnaryExpr {
        op,
        operand: Box::new(operand),
        span,
    })
}

/// Operator precedence levels for Pratt parsing.
///
/// Lower numbers bind less tightly (lower precedence).
/// Higher numbers bind more tightly (higher precedence).
///
/// Example: `1 + 2 * 3` parses as `1 + (2 * 3)` because
/// MULTIPLICATIVE (9) > ADDITIVE (8).
mod precedence {
    /// Logical OR: `||`
    pub const LOGICAL_OR: u16 = 1;
    /// Logical AND: `&&`
    pub const LOGICAL_AND: u16 = 2;
    /// Bitwise OR: `|`
    pub const BITWISE_OR: u16 = 3;
    /// Bitwise XOR: `^`
    pub const BITWISE_XOR: u16 = 4;
    /// Bitwise AND: `&`
    pub const BITWISE_AND: u16 = 5;
    /// Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
    pub const COMPARISON: u16 = 6;
    /// Shift: `<<`, `>>`
    pub const SHIFT: u16 = 7;
    /// Additive: `+`, `-`
    pub const ADDITIVE: u16 = 8;
    /// Multiplicative: `*`, `/`, `%`
    pub const MULTIPLICATIVE: u16 = 9;
    /// Unary prefix: `-`, `!`, `~`
    pub const UNARY: u16 = 10;
}

/// Expression parser with Pratt parsing for operator precedence
fn expr_parser<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(|expr| {
        // Atom parser - primary expressions
        let atom = atom_parser(expr.clone());

        // Build Pratt parser with precedence levels (see `precedence` module)
        atom.pratt((
            // Prefix operators
            prefix(
                precedence::UNARY,
                just(TokenKind::Minus).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::Neg, rhs, op_span),
            ),
            prefix(
                precedence::UNARY,
                just(TokenKind::Bang).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::Not, rhs, op_span),
            ),
            prefix(
                precedence::UNARY,
                just(TokenKind::Tilde).map_with(|_, e| e.span()),
                |op_span, rhs: Expr, _| make_unary(UnaryOp::BitNot, rhs, op_span),
            ),
            // Multiplicative: *, /, %
            infix(
                left(precedence::MULTIPLICATIVE),
                just(TokenKind::Star),
                |l, _, r, _| make_binary(l, BinaryOp::Mul, r),
            ),
            infix(
                left(precedence::MULTIPLICATIVE),
                just(TokenKind::Slash),
                |l, _, r, _| make_binary(l, BinaryOp::Div, r),
            ),
            infix(
                left(precedence::MULTIPLICATIVE),
                just(TokenKind::Percent),
                |l, _, r, _| make_binary(l, BinaryOp::Mod, r),
            ),
            // Additive: +, -
            infix(
                left(precedence::ADDITIVE),
                just(TokenKind::Plus),
                |l, _, r, _| make_binary(l, BinaryOp::Add, r),
            ),
            infix(
                left(precedence::ADDITIVE),
                just(TokenKind::Minus),
                |l, _, r, _| make_binary(l, BinaryOp::Sub, r),
            ),
            // Shift: <<, >>
            infix(
                left(precedence::SHIFT),
                just(TokenKind::LtLt),
                |l, _, r, _| make_binary(l, BinaryOp::Shl, r),
            ),
            infix(
                left(precedence::SHIFT),
                just(TokenKind::GtGt),
                |l, _, r, _| make_binary(l, BinaryOp::Shr, r),
            ),
            // Comparison: ==, !=, <, >, <=, >=
            infix(
                left(precedence::COMPARISON),
                just(TokenKind::EqEq),
                |l, _, r, _| make_binary(l, BinaryOp::Eq, r),
            ),
            infix(
                left(precedence::COMPARISON),
                just(TokenKind::BangEq),
                |l, _, r, _| make_binary(l, BinaryOp::Ne, r),
            ),
            infix(
                left(precedence::COMPARISON),
                just(TokenKind::Lt),
                |l, _, r, _| make_binary(l, BinaryOp::Lt, r),
            ),
            infix(
                left(precedence::COMPARISON),
                just(TokenKind::Gt),
                |l, _, r, _| make_binary(l, BinaryOp::Gt, r),
            ),
            infix(
                left(precedence::COMPARISON),
                just(TokenKind::LtEq),
                |l, _, r, _| make_binary(l, BinaryOp::Le, r),
            ),
            infix(
                left(precedence::COMPARISON),
                just(TokenKind::GtEq),
                |l, _, r, _| make_binary(l, BinaryOp::Ge, r),
            ),
            // Bitwise AND: &
            infix(
                left(precedence::BITWISE_AND),
                just(TokenKind::Amp),
                |l, _, r, _| make_binary(l, BinaryOp::BitAnd, r),
            ),
            // Bitwise XOR: ^
            infix(
                left(precedence::BITWISE_XOR),
                just(TokenKind::Caret),
                |l, _, r, _| make_binary(l, BinaryOp::BitXor, r),
            ),
            // Bitwise OR: |
            infix(
                left(precedence::BITWISE_OR),
                just(TokenKind::Pipe),
                |l, _, r, _| make_binary(l, BinaryOp::BitOr, r),
            ),
            // Logical AND: &&
            infix(
                left(precedence::LOGICAL_AND),
                just(TokenKind::AmpAmp),
                |l, _, r, _| make_binary(l, BinaryOp::And, r),
            ),
            // Logical OR: ||
            infix(
                left(precedence::LOGICAL_OR),
                just(TokenKind::PipePipe),
                |l, _, r, _| make_binary(l, BinaryOp::Or, r),
            ),
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

/// Parser for literal expressions: integers, strings, booleans, and unit
fn literal_parser<'src, I>() -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
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

    choice((int_lit, string_lit, bool_true, bool_false, unit_lit))
}

/// Parser for control flow expressions: break, continue, return, if, while, loop, match
fn control_flow_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Break
    let break_expr = select! {
        TokenKind::Break = e => Expr::Break(BreakExpr { span: to_rue_span(e.span()) }),
    };

    // Continue
    let continue_expr = select! {
        TokenKind::Continue = e => Expr::Continue(ContinueExpr { span: to_rue_span(e.span()) }),
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
            match_arm_parser(expr)
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

    choice((
        break_expr,
        continue_expr,
        return_expr,
        if_expr,
        while_expr,
        loop_expr,
        match_expr,
    ))
}

/// What can follow an identifier: call args, struct fields, path (::variant), path call (::fn()), or nothing
#[derive(Clone)]
enum IdentSuffix {
    Call(Vec<CallArg>),
    StructLit(Vec<FieldInit>),
    Path(Ident),                   // ::Variant (for enum variants)
    PathCall(Ident, Vec<CallArg>), // ::function() (for associated functions)
    None,
}

/// Parser for identifier-based expressions: identifiers, function calls, struct literals, and paths
fn call_and_access_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then(
            choice((
                // Function call: (args)
                call_args_parser(expr.clone())
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
                        call_args_parser(expr.clone())
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
        })
}

/// Suffix for field access (.field), method call (.method(args)), or indexing ([expr])
#[derive(Clone)]
enum Suffix {
    Field(Ident),
    /// Method call with method name, arguments, and closing paren position
    MethodCall(Ident, Vec<CallArg>, u32),
    /// Index expression with the inner expression and closing bracket position
    Index(Expr, u32),
}

/// Wraps a primary expression parser with field access, method call, and indexing suffixes
fn with_suffix_parser<'src, I>(
    primary: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone,
) -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Method call: .ident(args)
    let method_call_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then(
            call_args_parser(expr.clone())
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|(method, args), e| {
            Suffix::MethodCall(method, args, offset_to_u32(e.span().end))
        });

    // Field access: .ident (but NOT followed by ()
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then_ignore(none_of([TokenKind::LParen]).rewind())
        .map(Suffix::Field);

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|index, e| Suffix::Index(index, offset_to_u32(e.span().end)));

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
            Suffix::MethodCall(method, args, end) => {
                let span = Span::new(base.span().start, end);
                Expr::MethodCall(MethodCallExpr {
                    receiver: Box::new(base),
                    method,
                    args,
                    span,
                })
            }
            Suffix::Index(index, end) => {
                let span = Span::new(base.span().start, end);
                Expr::Index(IndexExpr {
                    base: Box::new(base),
                    index: Box::new(index),
                    span,
                })
            }
        },
    )
}

/// Atom parser - primary expressions (literals, identifiers, parens, blocks, control flow)
fn atom_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Self expression (in method bodies)
    let self_expr = select! {
        TokenKind::SelfValue = e => Expr::SelfExpr(SelfExpr { span: to_rue_span(e.span()) }),
    };

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
    // Note: literal_parser() includes unit_lit which must come before paren_expr
    // so () is parsed as unit, not empty parens
    // Note: self_expr must come before call_and_access_parser since self is a keyword
    let primary = choice((
        literal_parser(),
        control_flow_parser(expr.clone()),
        self_expr,
        intrinsic_call,
        array_lit,
        call_and_access_parser(expr.clone()),
        paren_expr,
        block_expr,
    ));

    // Wrap primary expressions with field access, method call, and indexing suffixes
    with_suffix_parser(primary, expr)
}

/// A block item is either a statement or an expression (potentially the final one)
#[derive(Debug, Clone)]
enum BlockItem {
    Statement(Statement),
    Expr(Expr),
}

/// What token follows an expression in a block (used to determine its role)
#[derive(Debug, Clone, Copy)]
enum ExprFollower {
    /// Semicolon follows - expression is a statement
    Semi,
    /// Right brace follows (not consumed) - expression is the final/return value
    RBrace,
    /// Some other token follows - only valid for control flow expressions
    Other,
    /// End of input - only valid for control flow expressions
    End,
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
                // Build up the expression from left to right, consuming suffixes by value
                let mut base_expr = Expr::Ident(base_ident);
                let mut suffixes = suffixes.into_iter().peekable();
                while let Some(suffix) = suffixes.next() {
                    let is_last = suffixes.peek().is_none();
                    if is_last {
                        // The last suffix determines the target type
                        return match suffix {
                            AssignSuffix::Field(field) => {
                                let span = Span::new(base_expr.span().start, field.span.end);
                                AssignTarget::Field(FieldExpr {
                                    base: Box::new(base_expr),
                                    field,
                                    span,
                                })
                            }
                            AssignSuffix::Index(index) => {
                                let span = Span::new(base_expr.span().start, index.span().end);
                                AssignTarget::Index(IndexExpr {
                                    base: Box::new(base_expr),
                                    index: Box::new(index),
                                    span,
                                })
                            }
                        };
                    }
                    // Build intermediate expressions
                    match suffix {
                        AssignSuffix::Field(field) => {
                            let span = Span::new(base_expr.span().start, field.span.end);
                            base_expr = Expr::Field(FieldExpr {
                                base: Box::new(base_expr),
                                field,
                                span,
                            });
                        }
                        AssignSuffix::Index(index) => {
                            let span = Span::new(base_expr.span().start, index.span().end);
                            base_expr = Expr::Index(IndexExpr {
                                base: Box::new(base_expr),
                                index: Box::new(index),
                                span,
                            });
                        }
                    }
                }
                // This is unreachable since we already checked suffixes.is_empty()
                unreachable!()
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

/// Returns true if the expression is a control flow construct or block that can appear
/// as a statement without a trailing semicolon.
///
/// This includes:
/// - Control flow: if, while, match, loop, break, continue, return
/// - Block expressions: { ... }
///
/// Block expressions are included because they are syntactically similar to control flow:
/// they are compound statements that naturally terminate without a semicolon.
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
            | Expr::Block(_)
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

/// Parses a single item within a block.
///
/// # Block Item Grammar
///
/// A block contains zero or more items. Each item is one of:
/// - **Let statement**: `let x = expr;` (always requires semicolon)
/// - **Assignment statement**: `target = expr;` (always requires semicolon)
/// - **Expression statement**: `expr;` (requires semicolon for most expressions)
/// - **Control flow statement**: `if/while/match/loop/break/continue/return ...`
///   (no semicolon needed when mid-block)
/// - **Final expression**: `expr` at the very end of a block (no semicolon, becomes
///   the block's return value)
///
/// # Parsing Strategy: Lookahead with `rewind()`
///
/// The challenge is distinguishing between:
/// 1. `{ foo; bar }` - `foo;` is a statement, `bar` is the final expression
/// 2. `{ if c { 1 } else { 2 } x }` - the `if` is a statement, `x` is final
/// 3. `{ if c { 1 } else { 2 } }` - the `if` IS the final expression
///
/// We use `rewind()` as a non-consuming lookahead to peek at what follows:
///
/// - `none_of([RBrace, Semi]).rewind()`: Succeeds if the NEXT token is neither
///   `}` nor `;`. The `.rewind()` means we check without consuming the token.
///   This identifies control flow in the middle of a block.
///
/// - `just(RBrace).rewind()`: Succeeds if the NEXT token is `}`. This identifies
///   the final expression of a block.
///
/// # Why `try_map()` for Control Flow?
///
/// After parsing an expression followed by a non-`}` non-`;` token, we need to
/// validate it's actually a control flow expression. If it's something like `x`
/// followed by `y`, that's a syntax error (missing semicolon). We use `try_map()`
/// to:
/// 1. Accept the parse if it's a control flow expression (valid without semicolon)
/// 2. Reject it otherwise, allowing chumsky to backtrack and try other branches
///
/// # Parse Order Matters
///
/// The `choice()` tries parsers in order. We must try:
/// 1. `let_stmt` first (starts with `let` keyword)
/// 2. `assign_stmt` second (identifier followed by `.`/`[` chain then `=`)
/// 3. `expr_with_semi` (any expression followed by `;`)
/// 4. `control_flow_stmt` (control flow NOT followed by `}` - mid-block)
/// 5. `final_expr` (any expression followed by `}` - end of block)
///
/// The assignment parser is tried before general expressions because `x = 5;`
/// could otherwise be misparsed as expression `x` followed by unexpected `=`.
fn block_item_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, extra::Err<Rich<'src, TokenKind>>> + Clone + 'src,
) -> impl Parser<'src, I, BlockItem, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Let statement: `let x: T = expr;`
    // Always requires a trailing semicolon.
    let let_stmt = let_statement_parser(expr.clone()).map(BlockItem::Statement);

    // Assignment statement: `target = expr;`
    // Target can be: variable (`x`), field (`a.b.c`), or index (`a[i]`).
    // Always requires a trailing semicolon.
    let assign_stmt = assign_statement_parser(expr.clone()).map(BlockItem::Statement);

    // Expression-based item: parse expression ONCE, then decide based on what follows.
    // This avoids O(2^n) complexity from repeatedly re-parsing expressions with backtracking.
    //
    // After parsing an expression, we check the next token:
    // - `;` → expression statement (value discarded)
    // - `}` → final expression (block's return value)
    // - other → mid-block control flow (only valid for if/while/match/loop/break/continue/return)
    let expr_item = expr
        .then(
            choice((
                just(TokenKind::Semi).to(ExprFollower::Semi),
                just(TokenKind::RBrace).rewind().to(ExprFollower::RBrace),
                any().rewind().to(ExprFollower::Other),
            ))
            .or(end().to(ExprFollower::End)),
        )
        .try_map(|(e, follower), span| match follower {
            ExprFollower::Semi => {
                // Expression followed by semicolon: `expr;`
                Ok(BlockItem::Statement(Statement::Expr(e)))
            }
            ExprFollower::RBrace => {
                // Final expression: `{ ... expr }`
                Ok(BlockItem::Expr(e))
            }
            ExprFollower::Other | ExprFollower::End => {
                // Mid-block control flow (no semicolon, not at end)
                // Only control flow expressions are valid here
                if is_control_flow_expr(&e) {
                    Ok(BlockItem::Statement(Statement::Expr(e)))
                } else {
                    Err(Rich::custom(span, "expected semicolon after expression"))
                }
            }
        });

    // Try parsers in order. Earlier parsers take precedence.
    // This order ensures:
    // - Keywords (`let`) are matched before being parsed as identifiers
    // - Assignments (`x = 5;`) are matched before `x` is parsed as an expression
    choice((let_stmt, assign_stmt, expr_item))
}

/// Process block items into statements and final expression
fn process_block_items(items: Vec<BlockItem>, block_span: Span) -> (Vec<Statement>, Expr) {
    let mut statements = Vec::new();
    let mut final_expr = None;

    for item in items {
        match item {
            BlockItem::Statement(stmt) => {
                // Had a non-semicolon expr before, but now we have more items
                // This shouldn't happen with correct grammar, but handle gracefully
                if let Some(e) = final_expr.take() {
                    statements.push(Statement::Expr(e));
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

/// Parser for struct definitions: [@directive]* struct Name { field: Type, ... }
fn struct_parser<'src, I>()
-> impl Parser<'src, I, StructDecl, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    directives_parser()
        .then(just(TokenKind::Struct).ignore_then(ident_parser()))
        .then(field_decls_parser().delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(|((directives, name), fields), e| StructDecl {
            directives,
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

/// Parser for drop fn declarations: drop fn TypeName(self) { body }
fn drop_fn_parser<'src, I>()
-> impl Parser<'src, I, DropFn, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse self parameter
    let self_param = just(TokenKind::SelfValue).map_with(|_, e| SelfParam {
        span: to_rue_span(e.span()),
    });

    just(TokenKind::Drop)
        .ignore_then(just(TokenKind::Fn))
        .ignore_then(ident_parser())
        .then(self_param.delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .then(block_parser(expr))
        .map_with(|((type_name, self_param), body), e| DropFn {
            type_name,
            self_param,
            body,
            span: to_rue_span(e.span()),
        })
}

/// Parser for top-level items (functions, structs, enums, impl blocks, and drop fns)
fn item_parser<'src, I>() -> impl Parser<'src, I, Item, extra::Err<Rich<'src, TokenKind>>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        function_parser().map(Item::Function),
        struct_parser().map(Item::Struct),
        enum_parser().map(Item::Enum),
        impl_parser().map(Item::Impl),
        drop_fn_parser().map(Item::DropFn),
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

/// Format a RichPattern for display in error messages.
fn format_pattern(pattern: &chumsky::error::RichPattern<'_, TokenKind>) -> String {
    use chumsky::error::RichPattern;
    match pattern {
        RichPattern::Token(tok) => tok.name().to_string(),
        RichPattern::Label(label) => label.to_string(),
        RichPattern::Identifier(ident) => format!("'{}'", ident),
        RichPattern::Any => "any token".to_string(),
        RichPattern::SomethingElse => "something else".to_string(),
        RichPattern::EndOfInput => "end of input".to_string(),
    }
}

/// Convert chumsky Rich error to CompileError, preserving rich context.
fn convert_error(err: Rich<'_, TokenKind>) -> CompileError {
    let span = to_rue_span(*err.span());

    // Build the base error from the reason
    let mut error = match err.reason() {
        chumsky::error::RichReason::ExpectedFound { expected, found } => {
            let expected_str: Cow<'static, str> = if expected.is_empty() {
                Cow::Borrowed("something")
            } else {
                Cow::Owned(
                    expected
                        .iter()
                        .take(3) // Limit to first 3 for readability
                        .map(format_pattern)
                        .collect::<Vec<_>>()
                        .join(" or "),
                )
            };

            let found_str: Cow<'static, str> = match found.as_ref() {
                Some(t) => Cow::Owned(t.name().to_string()),
                None => Cow::Borrowed("end of file"),
            };

            CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: expected_str,
                    found: found_str,
                },
                span,
            )
        }
        chumsky::error::RichReason::Custom(msg) => {
            // Preserve the custom error message directly
            CompileError::new(ErrorKind::ParseError(msg.clone()), span)
        }
    };

    // Add labelled contexts as secondary labels
    for (pattern, ctx_span) in err.contexts() {
        let label_msg = format!("while parsing {}", format_pattern(pattern));
        let label_span = to_rue_span(*ctx_span);
        error = error.with_label(label_msg, label_span);
    }

    error
}

/// Chumsky-based parser that converts tokens into an AST.
pub struct ChumskyParser {
    tokens: Vec<(TokenKind, SimpleSpan)>,
    source_len: usize,
    interner: ThreadedRodeo,
}

impl ChumskyParser {
    /// Create a new parser from tokens and an interner produced by the lexer.
    pub fn new(tokens: Vec<rue_lexer::Token>, interner: ThreadedRodeo) -> Self {
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
            interner,
        }
    }

    /// Parse the tokens into an AST, returning the AST and the interner.
    ///
    /// Returns all parse errors if parsing fails, not just the first one.
    pub fn parse(mut self) -> MultiErrorResult<(Ast, ThreadedRodeo)> {
        // Pre-intern primitive type symbols and store in thread-local
        let syms = PrimitiveTypeSpurs::new(&mut self.interner);
        PRIMITIVE_SYMS.with(|s| *s.borrow_mut() = Some(syms));

        // Create a stream from the token iterator
        let token_iter = self.tokens.iter().cloned();
        let stream = Stream::from_iter(token_iter);

        // Map the stream to split (Token, Span) tuples
        let eoi: SimpleSpan = (self.source_len..self.source_len).into();
        let mapped = stream.map(eoi, |(tok, span)| (tok, span));

        let result = ast_parser().parse(mapped).into_result().map_err(|errs| {
            let errors: Vec<CompileError> = errs.into_iter().map(convert_error).collect();
            CompileErrors::from(errors)
        });

        // Clear thread-local after parsing
        PRIMITIVE_SYMS.with(|s| *s.borrow_mut() = None);

        result.map(|ast| (ast, self.interner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    /// Result type for parsing that includes both the AST and interner.
    /// Provides convenient access to both the parsed AST and symbol resolution.
    #[derive(Debug)]
    struct ParseResult {
        ast: Ast,
        interner: ThreadedRodeo,
    }

    impl ParseResult {
        /// Get the string for a symbol.
        fn get(&self, sym: Spur) -> &str {
            self.interner.resolve(&sym)
        }
    }

    /// Result type for expression parsing that includes both the expr and interner.
    #[derive(Debug)]
    struct ExprResult {
        expr: Expr,
        interner: ThreadedRodeo,
    }

    impl ExprResult {
        /// Get the string for a symbol.
        fn get(&self, sym: Spur) -> &str {
            self.interner.resolve(&sym)
        }
    }

    fn parse(source: &str) -> MultiErrorResult<ParseResult> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().map_err(CompileErrors::from)?;
        let parser = ChumskyParser::new(tokens, interner);
        let (ast, interner) = parser.parse()?;
        Ok(ParseResult { ast, interner })
    }

    fn parse_expr(source: &str) -> MultiErrorResult<ExprResult> {
        let result = parse(&format!("fn main() -> i32 {{ {} }}", source))?;
        let interner = result.interner;
        let expr = match result.ast.items.into_iter().next().unwrap() {
            Item::Function(f) => match f.body {
                Expr::Block(block) => *block.expr,
                other => other,
            },
            Item::Struct(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Enum(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Impl(_) => panic!("parse_expr helper should only be used with functions"),
            Item::DropFn(_) => panic!("parse_expr helper should only be used with functions"),
        };
        Ok(ExprResult { expr, interner })
    }

    #[test]
    fn test_chumsky_parse_main() {
        let result = parse("fn main() -> i32 { 42 }").unwrap();

        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(result.get(f.name.name), "main");
                match f.return_type.as_ref().unwrap() {
                    TypeExpr::Named(ident) => assert_eq!(result.get(ident.name), "i32"),
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
            Item::DropFn(_) => panic!("expected Function"),
        }
    }

    #[test]
    fn test_chumsky_parse_addition() {
        let result = parse_expr("1 + 2").unwrap();
        match result.expr {
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
        let result = parse_expr("1 + 2 * 3").unwrap();
        match result.expr {
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
        let result = parse("fn main() -> i32 { let x = 42; x }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    assert_eq!(block.statements.len(), 1);
                    match &block.statements[0] {
                        Statement::Let(let_stmt) => {
                            assert!(!let_stmt.is_mut);
                            match &let_stmt.pattern {
                                LetPattern::Ident(ident) => {
                                    assert_eq!(result.get(ident.name), "x")
                                }
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
            Item::DropFn(_) => panic!("expected Function"),
        }
    }

    #[test]
    fn test_while_simple() {
        // Simplest while case
        let result = parse("fn main() -> i32 { while true { } 0 }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    #[test]
    fn test_while_with_statement() {
        // While with assignment
        let result = parse("fn main() -> i32 { while true { x = 1; } 0 }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    #[test]
    fn test_function_calls() {
        let result =
            parse("fn add(a: i32, b: i32) -> i32 { a + b } fn main() -> i32 { add(1, 2) }")
                .unwrap();
        assert_eq!(result.ast.items.len(), 2);
    }

    #[test]
    fn test_if_else() {
        let result = parse("fn main() -> i32 { if true { 1 } else { 0 } }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    #[test]
    fn test_nested_control_flow() {
        let result =
            parse("fn main() -> i32 { let mut x = 0; while x < 10 { x = x + 1; } x }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
    }

    // ==================== Impl Block and Method Parsing Tests ====================

    #[test]
    fn test_impl_block_empty() {
        let result = parse("struct Point { x: i32, y: i32 } impl Point {}").unwrap();
        assert_eq!(result.ast.items.len(), 2);
        match &result.ast.items[1] {
            Item::Impl(impl_block) => {
                assert_eq!(result.get(impl_block.type_name.name), "Point");
                assert!(impl_block.methods.is_empty());
            }
            _ => panic!("expected Impl"),
        }
    }

    #[test]
    fn test_impl_block_single_method() {
        let result =
            parse("struct Point { x: i32 } impl Point { fn get_x(self) -> i32 { self.x } }")
                .unwrap();
        assert_eq!(result.ast.items.len(), 2);
        match &result.ast.items[1] {
            Item::Impl(impl_block) => {
                assert_eq!(result.get(impl_block.type_name.name), "Point");
                assert_eq!(impl_block.methods.len(), 1);
                let method = &impl_block.methods[0];
                assert_eq!(result.get(method.name.name), "get_x");
                assert!(method.receiver.is_some()); // has self
                assert!(method.params.is_empty()); // no additional params
            }
            _ => panic!("expected Impl"),
        }
    }

    #[test]
    fn test_impl_block_method_with_params() {
        let result = parse(
            "struct Point { x: i32 } impl Point { fn add(self, n: i32) -> i32 { self.x + n } }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 2);
        match &result.ast.items[1] {
            Item::Impl(impl_block) => {
                let method = &impl_block.methods[0];
                assert_eq!(result.get(method.name.name), "add");
                assert!(method.receiver.is_some());
                assert_eq!(method.params.len(), 1);
                assert_eq!(result.get(method.params[0].name.name), "n");
            }
            _ => panic!("expected Impl"),
        }
    }

    #[test]
    fn test_impl_block_associated_function() {
        // Associated function (no self)
        let result = parse(
            "struct Point { x: i32, y: i32 } impl Point { fn new(x: i32, y: i32) -> Point { Point { x: x, y: y } } }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 2);
        match &result.ast.items[1] {
            Item::Impl(impl_block) => {
                let method = &impl_block.methods[0];
                assert_eq!(result.get(method.name.name), "new");
                assert!(method.receiver.is_none()); // no self
                assert_eq!(method.params.len(), 2);
            }
            _ => panic!("expected Impl"),
        }
    }

    #[test]
    fn test_impl_block_multiple_methods() {
        let result = parse(
            "struct Counter { value: i32 }
             impl Counter {
                 fn new() -> Counter { Counter { value: 0 } }
                 fn get(self) -> i32 { self.value }
                 fn increment(self) -> i32 { self.value + 1 }
             }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 2);
        match &result.ast.items[1] {
            Item::Impl(impl_block) => {
                assert_eq!(impl_block.methods.len(), 3);
                // First is associated function (no self)
                assert!(impl_block.methods[0].receiver.is_none());
                assert_eq!(result.get(impl_block.methods[0].name.name), "new");
                // Second is method (has self)
                assert!(impl_block.methods[1].receiver.is_some());
                assert_eq!(result.get(impl_block.methods[1].name.name), "get");
                // Third is method (has self)
                assert!(impl_block.methods[2].receiver.is_some());
                assert_eq!(result.get(impl_block.methods[2].name.name), "increment");
            }
            _ => panic!("expected Impl"),
        }
    }

    #[test]
    fn test_impl_method_with_directive() {
        let result =
            parse("struct Foo {} impl Foo { @inline fn bar(self) -> i32 { 42 } }").unwrap();
        match &result.ast.items[1] {
            Item::Impl(impl_block) => {
                let method = &impl_block.methods[0];
                assert_eq!(method.directives.len(), 1);
                assert_eq!(result.get(method.directives[0].name.name), "inline");
            }
            _ => panic!("expected Impl"),
        }
    }

    // ==================== Method Call Parsing Tests ====================

    #[test]
    fn test_method_call_simple() {
        let result = parse_expr("x.foo()").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "foo");
                assert!(call.args.is_empty());
                match call.receiver.as_ref() {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "x"),
                    _ => panic!("expected Ident receiver"),
                }
            }
            _ => panic!("expected MethodCall, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_method_call_with_args() {
        let result = parse_expr("point.add(5, 10)").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "add");
                assert_eq!(call.args.len(), 2);
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_method_call_chained() {
        let result = parse_expr("x.foo().bar()").unwrap();
        match &result.expr {
            Expr::MethodCall(outer) => {
                assert_eq!(result.get(outer.method.name), "bar");
                match outer.receiver.as_ref() {
                    Expr::MethodCall(inner) => {
                        assert_eq!(result.get(inner.method.name), "foo");
                    }
                    _ => panic!("expected inner MethodCall"),
                }
            }
            _ => panic!("expected outer MethodCall"),
        }
    }

    #[test]
    fn test_method_call_on_field_access() {
        let result = parse_expr("obj.field.method()").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "method");
                match call.receiver.as_ref() {
                    Expr::Field(field) => {
                        assert_eq!(result.get(field.field.name), "field");
                    }
                    _ => panic!("expected Field receiver"),
                }
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_field_access_not_method_call() {
        // .field (not followed by parens) should parse as FieldExpr, not MethodCall
        let result = parse_expr("x.field").unwrap();
        match &result.expr {
            Expr::Field(field) => {
                assert_eq!(result.get(field.field.name), "field");
            }
            _ => panic!("expected Field, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_method_call_on_struct_literal() {
        let result =
            parse("struct Point { x: i32 } fn main() -> i32 { Point { x: 1 }.get() }").unwrap();
        match &result.ast.items[1] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::MethodCall(call) => {
                        assert_eq!(result.get(call.method.name), "get");
                        match call.receiver.as_ref() {
                            Expr::StructLit(lit) => {
                                assert_eq!(result.get(lit.name.name), "Point")
                            }
                            _ => panic!("expected StructLit receiver"),
                        }
                    }
                    _ => panic!("expected MethodCall"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_method_call_on_paren_expr() {
        let result = parse_expr("(x).method()").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(result.get(call.method.name), "method");
                match call.receiver.as_ref() {
                    Expr::Paren(_) => {}
                    _ => panic!("expected Paren receiver"),
                }
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_method_call_mixed_with_index() {
        // Complex chain: array[0].method().field
        let result = parse_expr("arr[0].get().value").unwrap();
        match &result.expr {
            Expr::Field(field) => {
                assert_eq!(result.get(field.field.name), "value");
                match field.base.as_ref() {
                    Expr::MethodCall(call) => {
                        assert_eq!(result.get(call.method.name), "get");
                        match call.receiver.as_ref() {
                            Expr::Index(_) => {}
                            _ => panic!("expected Index receiver"),
                        }
                    }
                    _ => panic!("expected MethodCall"),
                }
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn test_associated_function_call() {
        // Type::function(args) syntax
        let result = parse_expr("Point::new(1, 2)").unwrap();
        match &result.expr {
            Expr::AssocFnCall(call) => {
                assert_eq!(result.get(call.type_name.name), "Point");
                assert_eq!(result.get(call.function.name), "new");
                assert_eq!(call.args.len(), 2);
            }
            _ => panic!("expected AssocFnCall, got {:?}", result.expr),
        }
    }

    // ==================== Borrow Parameter Parsing Tests ====================

    #[test]
    fn test_borrow_param_simple() {
        // Function with a borrow parameter
        let result = parse("fn read(borrow x: i32) -> i32 { x }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "x");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_with_struct_type() {
        // Borrow parameter with a user-defined type
        let result =
            parse("struct Point { x: i32 } fn read(borrow p: Point) -> i32 { p.x }").unwrap();
        match &result.ast.items[1] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "p");
                match &f.params[0].ty {
                    TypeExpr::Named(ident) => assert_eq!(result.get(ident.name), "Point"),
                    _ => panic!("expected Named type"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_mixed_with_normal() {
        // Mixed borrow and normal parameters
        let result = parse("fn add(borrow a: i32, b: i32) -> i32 { a + b }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "a");
                assert_eq!(f.params[1].mode, ParamMode::Normal);
                assert_eq!(result.get(f.params[1].name.name), "b");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_mixed_with_inout() {
        // Borrow and inout parameters in the same function
        let result = parse("fn modify(borrow a: i32, inout b: i32) { b = a; }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(result.get(f.params[0].name.name), "a");
                assert_eq!(f.params[1].mode, ParamMode::Inout);
                assert_eq!(result.get(f.params[1].name.name), "b");
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_with_array_type() {
        // Borrow parameter with array type
        let result = parse("fn first(borrow arr: [i32; 3]) -> i32 { arr[0] }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                match &f.params[0].ty {
                    TypeExpr::Array { length, .. } => assert_eq!(*length, 3),
                    _ => panic!("expected Array type"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_borrow_param_multiple() {
        // Multiple borrow parameters
        let result = parse("fn sum(borrow a: i32, borrow b: i32) -> i32 { a + b }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                assert_eq!(f.params[1].mode, ParamMode::Borrow);
            }
            _ => panic!("expected Function"),
        }
    }

    // ==================== Borrow Argument Parsing Tests ====================

    #[test]
    fn test_borrow_arg_simple() {
        // Function call with a borrow argument
        let result = parse_expr("read(borrow x)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(result.get(call.name.name), "read");
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                match &call.args[0].expr {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "x"),
                    _ => panic!("expected Ident argument"),
                }
            }
            _ => panic!("expected Call, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_borrow_arg_mixed_with_normal() {
        // Mixed borrow and normal arguments
        let result = parse_expr("foo(borrow a, b)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 2);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                assert_eq!(call.args[1].mode, ArgMode::Normal);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_mixed_with_inout() {
        // Borrow and inout arguments in the same call
        let result = parse_expr("modify(borrow a, inout b)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 2);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                assert_eq!(call.args[1].mode, ArgMode::Inout);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_multiple() {
        // Multiple borrow arguments
        let result = parse_expr("sum(borrow x, borrow y)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 2);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                assert_eq!(call.args[1].mode, ArgMode::Borrow);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_with_field_access() {
        // Borrow argument with field access expression
        let result = parse_expr("read(borrow point.x)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
                match &call.args[0].expr {
                    Expr::Field(field) => assert_eq!(result.get(field.field.name), "x"),
                    _ => panic!("expected Field expression"),
                }
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn test_borrow_arg_in_method_call() {
        // Borrow argument in method call
        let result = parse_expr("obj.method(borrow x)").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_borrow_arg_in_associated_function() {
        // Borrow argument in associated function call
        let result = parse_expr("Foo::bar(borrow x)").unwrap();
        match &result.expr {
            Expr::AssocFnCall(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
            }
            _ => panic!("expected AssocFnCall"),
        }
    }

    #[test]
    fn test_borrow_helper_methods() {
        // Test CallArg helper methods
        let result = parse_expr("foo(borrow x, inout y, z)").unwrap();
        match &result.expr {
            Expr::Call(call) => {
                assert!(call.args[0].is_borrow());
                assert!(!call.args[0].is_inout());
                assert!(call.args[1].is_inout());
                assert!(!call.args[1].is_borrow());
                assert!(!call.args[2].is_borrow());
                assert!(!call.args[2].is_inout());
            }
            _ => panic!("expected Call"),
        }
    }

    // ==================== Block Expression Statement Tests ====================

    #[test]
    fn test_block_statement_followed_by_identifier() {
        // Block expression as a statement followed by an identifier expression
        // This is the regression test for rue-wo1g
        let result = parse(
            "fn main() -> i32 {
                let a = 1;
                {
                    let b = 2;
                }
                a
            }",
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    // Should have 2 statements: let a = 1; and { let b = 2; }
                    assert_eq!(block.statements.len(), 2);
                    // First statement is a let
                    assert!(matches!(&block.statements[0], Statement::Let(_)));
                    // Second statement is an expression statement containing a block
                    match &block.statements[1] {
                        Statement::Expr(Expr::Block(_)) => {}
                        _ => panic!("expected Expr(Block), got {:?}", block.statements[1]),
                    }
                    // Final expression should be 'a' (simple identifier)
                    match block.expr.as_ref() {
                        Expr::Ident(ident) => assert_eq!(result.get(ident.name), "a"),
                        _ => panic!("expected Ident expression, got {:?}", block.expr),
                    }
                }
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    // ==================== Error Conversion Tests ====================

    #[test]
    fn test_error_preserves_span() {
        // Parse invalid syntax and verify error has span information
        let result = parse("fn main() -> i32 { let = 42; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        // Get the first error and verify it has span information
        let error = errors.first().expect("should have at least one error");
        assert!(error.has_span());
        assert!(error.span().is_some());
    }

    #[test]
    fn test_error_expected_found() {
        // Missing expression after let
        let result = parse("fn main() -> i32 { let x = ; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let error = errors.first().expect("should have at least one error");
        // Error message should describe what was expected vs found
        let msg = error.to_string();
        assert!(
            msg.contains("expected") || msg.contains("found"),
            "error message: {}",
            msg
        );
    }

    #[test]
    fn test_error_unexpected_eof() {
        // Unterminated block
        let result = parse("fn main() -> i32 {");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let error = errors.first().expect("should have at least one error");
        let msg = error.to_string();
        // Should indicate end of file was reached unexpectedly
        assert!(
            msg.contains("end of file") || msg.contains("expected"),
            "error message: {}",
            msg
        );
    }

    #[test]
    fn test_format_pattern_token() {
        use chumsky::error::RichPattern;
        use chumsky::util::MaybeRef;

        let pattern = RichPattern::Token(MaybeRef::Val(TokenKind::Plus));
        let formatted = format_pattern(&pattern);
        // TokenKind::name() returns quoted form like "'+'"
        assert_eq!(formatted, "'+'");
    }

    #[test]
    fn test_format_pattern_label() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::Label(Cow::Borrowed("expression"));
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "expression");
    }

    #[test]
    fn test_format_pattern_identifier() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::Identifier("while".to_string());
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "'while'");
    }

    #[test]
    fn test_format_pattern_any() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::Any;
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "any token");
    }

    #[test]
    fn test_format_pattern_end_of_input() {
        use chumsky::error::RichPattern;

        let pattern: RichPattern<'_, TokenKind> = RichPattern::EndOfInput;
        let formatted = format_pattern(&pattern);
        assert_eq!(formatted, "end of input");
    }

    #[test]
    fn test_parse_error_no_empty_found_clause() {
        // Test that error messages don't have empty "found" clauses
        // This was a bug when Custom errors were mapped to UnexpectedToken with empty found
        let result = parse("fn main() -> i32 { let x = ; }");
        assert!(result.is_err());
        let errors = result.unwrap_err();
        let error = errors.first().expect("should have at least one error");
        let msg = error.to_string();
        // Should not end with "found " (empty found)
        assert!(
            !msg.ends_with("found "),
            "error message should not have trailing empty 'found': {}",
            msg
        );
    }

    #[test]
    fn test_parse_error_variant_display() {
        // Directly test that ParseError displays correctly
        let error = CompileError::new(
            ErrorKind::ParseError("expected semicolon after expression".to_string()),
            rue_span::Span::new(0, 10),
        );
        assert_eq!(error.to_string(), "expected semicolon after expression");
    }

    #[test]
    fn test_offset_to_u32_normal() {
        // Normal values should convert without issue
        assert_eq!(offset_to_u32(0), 0);
        assert_eq!(offset_to_u32(42), 42);
        assert_eq!(offset_to_u32(1000000), 1000000);
        assert_eq!(offset_to_u32(u32::MAX as usize), u32::MAX);
    }

    #[test]
    #[should_panic(expected = "offset 4294967296 exceeds u32::MAX")]
    #[cfg(debug_assertions)]
    fn test_offset_to_u32_overflow_panics_in_debug() {
        // Value just over u32::MAX should panic in debug builds
        let _ = offset_to_u32((u32::MAX as usize) + 1);
    }

    #[test]
    fn test_to_rue_span_normal() {
        // Normal spans should convert without issue
        let simple = SimpleSpan::new(10, 20);
        let rue = to_rue_span(simple);
        assert_eq!(rue.start, 10);
        assert_eq!(rue.end, 20);
    }

    #[test]
    fn test_parse_returns_multiple_errors() {
        // Test that we return ALL parse errors, not just the first one.
        // This uses source with multiple syntax errors that can be detected at once.
        // Note: The actual number of errors depends on Chumsky's error recovery,
        // but this test ensures the infrastructure returns all errors it finds.
        let source = "fn main() { let }"; // Missing variable name and expression

        let result = parse(source);
        assert!(result.is_err(), "Expected parsing to fail");

        // We should get at least one error
        let errors = result.unwrap_err();
        assert!(
            !errors.is_empty(),
            "Expected at least one error but got none"
        );

        // Verify we can iterate over all errors
        let error_count = errors.len();
        assert!(
            error_count >= 1,
            "Expected at least 1 error, got {}",
            error_count
        );
    }

    #[test]
    fn test_parse_error_collection_preserves_all() {
        // Test that the error collection mechanism preserves all errors.
        // This directly tests the CompileErrors::from(Vec<CompileError>) path.
        let errors = vec![
            CompileError::without_span(ErrorKind::UnexpectedToken {
                expected: std::borrow::Cow::Borrowed("ident"),
                found: std::borrow::Cow::Borrowed("let"),
            }),
            CompileError::without_span(ErrorKind::UnexpectedToken {
                expected: std::borrow::Cow::Borrowed("expr"),
                found: std::borrow::Cow::Borrowed("rbrace"),
            }),
        ];

        let compile_errors = CompileErrors::from(errors);
        assert_eq!(compile_errors.len(), 2, "Expected 2 errors to be preserved");
    }
}
