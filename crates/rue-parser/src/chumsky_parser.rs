//! Chumsky-based parser for the Rue programming language.
//!
//! This module provides a parser implementation using chumsky combinators
//! with Pratt parsing for expression precedence.

use crate::ast::{
    AnonStructField, ArgMode, ArrayLength, ArrayLitExpr, AssignStatement, AssignTarget, Ast,
    BinaryExpr, BinaryOp, BlockExpr, BoolLit, BreakExpr, CallArg, CallExpr, CheckedBlockExpr,
    ComptimeBlockExpr, ConstDecl, ContinueExpr, Directive, DirectiveArg, Directives, DropFn,
    EnumDecl, EnumVariant, Expr, FieldDecl, FieldExpr, FieldInit, ForExpr, Function, Ident, IfExpr,
    IndexExpr, IntLit, IntrinsicArg, IntrinsicCallExpr, Item, LetPattern, LetStatement, LoopExpr,
    MatchArm, MatchExpr, Method, MethodCallExpr, NegIntLit, Param, ParamMode, ParenExpr,
    PathPattern, Pattern, ReturnExpr, SelfExpr, SelfParam, Statement, StringLit, StructDecl,
    StructLitExpr, TryExpr, TypeExpr, TypeLitExpr, UnaryExpr, UnaryOp, UnitLit, Visibility,
    WhileExpr,
};
use chumsky::input::{Input as ChumskyInput, MapExtra, Stream, ValueInput};
use chumsky::pratt::{infix, left, prefix};
use chumsky::prelude::*;
use chumsky::recovery::via_parser;
use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, ErrorKind, MAX_NESTING_DEPTH, MultiErrorResult};
use rue_lexer::TokenKind;
use rue_span::{FileId, Span};
use std::borrow::Cow;

use chumsky::extra::SimpleState;

/// Pre-interned symbols for primitive type names and special keywords.
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
    /// Self type keyword - used in methods to refer to the containing struct type
    pub self_type: Spur,
    /// The keyword `type` — the compile-time type of types (spec 2.4:3). The
    /// lexer emits a dedicated `Type` token (not an `Ident`), so the type-position
    /// parser maps that token back to this interned "type" symbol, keeping sema's
    /// `Type::from_primitive_name("type")` / `comptime T: type` resolution
    /// unchanged. (RUE-331)
    pub type_kw: Spur,
    /// The identifier `as` — not a Rue keyword, but recognized after an
    /// expression to give Rust users a targeted "no 'as' operator" error
    /// instead of a generic parse error. (RUE-133)
    pub as_kw: Spur,
    /// The keyword `drop` — a reserved keyword (for `drop fn`) that is ALSO
    /// the name of the `@drop(x)` intrinsic (RUE-187). Because the lexer emits
    /// a dedicated `Drop` token rather than an `Ident`, the intrinsic-name
    /// parser maps that token back to this interned "drop" symbol.
    pub drop_kw: Spur,
    /// The synthetic method name `__drop` used for a destructor declared
    /// *inside* an anonymous (comptime-fn) struct body (`drop fn(self) { … }`).
    /// A named struct's destructor is a top-level `drop fn Name(self)` keyed by
    /// the struct name; an anonymous struct has no name, so its destructor is
    /// carried as an ordinary method under this reserved name and later
    /// registered as the struct's `{name}.__drop` destructor (RUE-312).
    pub drop_marker: Spur,
    /// Known directive name `allow`. Pre-interned so expression-position
    /// `@allow(...)` can be rejected as a misplaced directive (RUE-403).
    pub allow_directive: Spur,
    /// Known directive name `copy`. Pre-interned so expression-position
    /// `@copy(...)` can be rejected as a misplaced directive (RUE-403).
    pub copy_directive: Spur,
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
            self_type: interner.get_or_intern("Self"),
            type_kw: interner.get_or_intern("type"),
            as_kw: interner.get_or_intern("as"),
            drop_kw: interner.get_or_intern("drop"),
            drop_marker: interner.get_or_intern("__drop"),
            allow_directive: interner.get_or_intern("allow"),
            copy_directive: interner.get_or_intern("copy"),
        }
    }
}

/// Parser state containing primitive type symbols and file ID.
///
/// This struct holds mutable state needed during parsing:
/// - Pre-interned primitive type symbols for efficient lookup
/// - The FileId for the current file being parsed (for multi-file compilation)
#[derive(Clone, Copy)]
pub struct ParserState {
    /// Pre-interned primitive type symbols.
    pub syms: PrimitiveTypeSpurs,
    /// The file ID for spans in this file.
    pub file_id: FileId,
}

impl ParserState {
    /// Create a new parser state with the given symbols and file ID.
    pub fn new(syms: PrimitiveTypeSpurs, file_id: FileId) -> Self {
        Self { syms, file_id }
    }
}

/// Type alias for parser extras that carries parser state.
/// This replaces the previous thread-local approach with compile-time safe state passing.
type ParserExtras<'src> = extra::Full<Rich<'src, TokenKind>, SimpleState<ParserState>, ()>;

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

/// Convert chumsky SimpleSpan to rue_span::Span with a specific file ID.
///
/// # Panics
///
/// In debug builds, panics if `span.start` or `span.end` exceeds `u32::MAX`.
/// This would only happen for source files larger than 4GB.
fn to_rue_span_with_file(span: SimpleSpan, file_id: FileId) -> Span {
    Span::with_file(file_id, offset_to_u32(span.start), offset_to_u32(span.end))
}

/// Extract a Span with file ID from the parser extra.
/// This is the primary way to create spans during parsing.
#[inline]
fn span_from_extra<'src, I>(e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>) -> Span
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let file_id = e.state().0.file_id;
    to_rue_span_with_file(e.span(), file_id)
}

/// Parser that produces Ident from identifier tokens
fn ident_parser<'src, I>() -> impl Parser<'src, I, Ident, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    select! {
        TokenKind::Ident(name) = e => Ident {
            name,
            span: span_from_extra(e),
        },
    }
    // A bare `select!` filter cannot enumerate its accepted tokens, so on
    // failure chumsky would report the useless catch-all "expected something
    // else". `ident_parser` is used pervasively (parameter/field/method names,
    // directive and intrinsic names, ...), so naming what is wanted here fixes
    // that message across the whole grammar. (RUE-19)
    .labelled("identifier")
}

/// Parser for the `?` (try) postfix suffix (RUE-6, ADR-0038). Kept as a
/// standalone helper (rather than an inline branch) so its input type is
/// pinned by the return-type annotation. Emits [`Suffix::Try`] carrying the
/// offset just past the `?` token.
fn question_suffix_parser<'src, I>() -> impl Parser<'src, I, Suffix, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    select! {
        TokenKind::Question = e => Suffix::Try(span_from_extra(e).end),
    }
}

/// Parser for primitive type keywords: i8, i16, i32, i64, u8, u16, u32, u64, bool
/// These are reserved keywords that cannot be used as identifiers.
fn primitive_type_parser<'src, I>() -> impl Parser<'src, I, TypeExpr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Access primitive type symbols from parser state via e.state().0
    // SimpleState<T> wraps T in a .0 field
    // Type annotation on closure parameter is needed to help Rust infer the Extra type
    let i8_parser =
        just(TokenKind::I8).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i8,
                span: span_from_extra(e),
            })
        });
    let i16_parser =
        just(TokenKind::I16).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i16,
                span: span_from_extra(e),
            })
        });
    let i32_parser =
        just(TokenKind::I32).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i32,
                span: span_from_extra(e),
            })
        });
    let i64_parser =
        just(TokenKind::I64).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.i64,
                span: span_from_extra(e),
            })
        });
    let u8_parser =
        just(TokenKind::U8).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u8,
                span: span_from_extra(e),
            })
        });
    let u16_parser =
        just(TokenKind::U16).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u16,
                span: span_from_extra(e),
            })
        });
    let u32_parser =
        just(TokenKind::U32).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u32,
                span: span_from_extra(e),
            })
        });
    let u64_parser =
        just(TokenKind::U64).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.u64,
                span: span_from_extra(e),
            })
        });
    let bool_parser =
        just(TokenKind::Bool).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            let syms = e.state().0.syms;
            TypeExpr::Named(Ident {
                name: syms.bool,
                span: span_from_extra(e),
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
fn type_parser<'src, I>() -> impl Parser<'src, I, TypeExpr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(move |ty| {
        // Unit type: ()
        let unit_type = just(TokenKind::LParen)
            .then(just(TokenKind::RParen))
            .map_with(|_, e| TypeExpr::Unit(span_from_extra(e)));

        // Never type: !
        let never_type = just(TokenKind::Bang).map_with(|_, e| TypeExpr::Never(span_from_extra(e)));

        // Array type: [T; N] where N is an integer literal, a name referring
        // to a file-level `const` or a `comptime` value parameter (RUE-16), or
        // a comptime-evaluable call `f(args)` (RUE-309). The call form is a
        // recursive grammar so arguments (literals, names, or nested calls)
        // compose; it is tried before the bare-name form so `f(...)` is not
        // mis-parsed as a name `f` with a dangling argument list.
        let array_length = recursive(|array_length| {
            let call = ident_parser()
                .then(
                    array_length
                        .separated_by(just(TokenKind::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
                )
                .map(|(name, args)| ArrayLength::Call { name, args });
            choice((
                select! { TokenKind::Int(n) => ArrayLength::Literal(n as u64) },
                call,
                ident_parser().map(ArrayLength::Named),
            ))
            // Otherwise an empty/invalid length reports the bare-`select!`
            // catch-all "expected something else". (RUE-19)
            .labelled("array length")
        });
        let array_type = just(TokenKind::LBracket)
            .ignore_then(ty.clone())
            .then_ignore(just(TokenKind::Semi))
            .then(array_length)
            .then_ignore(just(TokenKind::RBracket))
            .map_with(|(element, length), e| TypeExpr::Array {
                element: Box::new(element),
                length,
                span: span_from_extra(e),
            });

        // Slice type: `[T]` (no length) — a second-class fat-pointer view
        // (ADR-0043, RUE-322). Tried after `array_type` so `[T; N]` is not
        // mis-parsed; the two diverge on whether a `;` follows the element.
        let slice_type = just(TokenKind::LBracket)
            .ignore_then(ty.clone())
            .then_ignore(just(TokenKind::RBracket))
            .map_with(|element, e| TypeExpr::Slice {
                element: Box::new(element),
                span: span_from_extra(e),
            });

        // Anonymous struct type: struct { field: Type, ... }
        // Used in comptime type construction
        let anon_struct_field = ident_parser()
            .then_ignore(just(TokenKind::Colon))
            .then(ty.clone())
            .map_with(|(name, field_ty), e| AnonStructField {
                name,
                ty: field_ty,
                span: span_from_extra(e),
            });

        let anon_struct_type = just(TokenKind::Struct)
            .ignore_then(just(TokenKind::LBrace))
            .ignore_then(
                anon_struct_field
                    .separated_by(just(TokenKind::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(TokenKind::RBrace))
            .map_with(|fields, e| TypeExpr::AnonymousStruct {
                fields,
                methods: vec![],
                span: span_from_extra(e),
            });

        // Pointer type: ptr const T or ptr mut T
        let ptr_const_type = just(TokenKind::Ptr)
            .ignore_then(just(TokenKind::Const))
            .ignore_then(ty.clone())
            .map_with(|pointee, e| TypeExpr::PointerConst {
                pointee: Box::new(pointee),
                span: span_from_extra(e),
            });

        let ptr_mut_type = just(TokenKind::Ptr)
            .ignore_then(just(TokenKind::Mut))
            .ignore_then(ty.clone())
            .map_with(|pointee, e| TypeExpr::PointerMut {
                pointee: Box::new(pointee),
                span: span_from_extra(e),
            });

        // Type-function application in type position: `Name(arg, ...)`
        // (RUE-241). Calls a comptime `-> type` constructor with type
        // arguments; e.g. `Result(i32, i32)`. Arguments are themselves types
        // (parsed via the recursive `ty`), so nested calls compose
        // (`Result(Option(i32), i32)`). Must precede `named_type` in the
        // choice so `Name(...)` is not mis-parsed as a bare `Name` leaving the
        // argument list dangling.
        // A type-call ARGUMENT is a type, or an integer literal bound to a
        // comptime VALUE parameter (`Buffer(2)`, `Matrix(2, -1)`; RUE-552).
        // The literal is canonicalized into the call's type string by astgen,
        // exactly like `Str(8)`'s dedicated rule below.
        let type_call_arg = choice((
            just(TokenKind::Minus)
                .or_not()
                .then(select! { TokenKind::Int(n) => n })
                .map_with(|(neg, n), e| TypeExpr::IntArg {
                    value: if neg.is_some() {
                        -(n as i128)
                    } else {
                        n as i128
                    },
                    span: span_from_extra(e),
                }),
            ty.clone(),
        ));

        let type_call = ident_parser()
            .then(
                type_call_arg
                    .clone()
                    .separated_by(just(TokenKind::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
            )
            .map_with(|(name, args), e| TypeExpr::TypeCall {
                name,
                args,
                span: span_from_extra(e),
            });

        // Module-qualified type-function application in type position:
        // `std.option.Option(i64)` (RUE-419). Keep this distinct from the
        // existing bare type-call node so downstream stages can resolve the
        // module prefix before applying the constructor.
        let qualified_type_call = ident_parser()
            .separated_by(just(TokenKind::Dot))
            .at_least(2)
            .collect::<Vec<_>>()
            .then(
                type_call_arg
                    .separated_by(just(TokenKind::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
            )
            .map_with(|(segments, args), e| TypeExpr::QualifiedTypeCall {
                segments,
                args,
                span: span_from_extra(e),
            });

        // Fixed-capacity string type with a LITERAL capacity: `Str(8)`
        // (ADR-0043 Phase 5, RUE-326). The integer capacity is not a type, so
        // it cannot ride the `type_call` grammar (whose args are types); this
        // dedicated rule matches `ident ( INT )`. Placed AFTER `type_call` in
        // the choice so the const-capacity spelling `Str(N)` (where `N` names a
        // `const`) still parses as a `TypeCall` with a `Named` type argument and
        // reduces to the same canonical `Str(N)` type name. `name` is kept so a
        // non-`Str` callee resolves to a clean unknown-type error.
        let str_fixed_type = ident_parser()
            .then(
                select! { TokenKind::Int(n) => n as u64 }
                    .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
            )
            .map_with(|(name, length), e| TypeExpr::StrFixed {
                name,
                length,
                span: span_from_extra(e),
            });

        // Named type: user-defined types like MyStruct
        let named_type = ident_parser().map(TypeExpr::Named);

        // Module-qualified named type in type position: `lib.Point` (RUE-419).
        let qualified_type = ident_parser()
            .separated_by(just(TokenKind::Dot))
            .at_least(2)
            .collect::<Vec<_>>()
            .map_with(|segments, e| TypeExpr::Qualified {
                segments,
                span: span_from_extra(e),
            });

        // Self type: Self keyword used in methods to refer to the containing struct
        let self_type = just(TokenKind::SelfType).map_with(|_, e| {
            let span = span_from_extra(e);
            TypeExpr::Named(Ident {
                name: e.state().syms.self_type,
                span,
            })
        });

        // `type` keyword: the compile-time type of types (spec 2.4:3), used in
        // type position as `comptime T: type` and `-> type`. The lexer now emits
        // a reserved `Type` token rather than an identifier (RUE-331), so map it
        // back to the interned "type" name — downstream sema resolves it via
        // `Type::from_primitive_name("type")` exactly as when it was an ident.
        let comptime_type = just(TokenKind::Type).map_with(|_, e| {
            let span = span_from_extra(e);
            TypeExpr::Named(Ident {
                name: e.state().syms.type_kw,
                span,
            })
        });

        choice((
            unit_type,
            never_type,
            array_type,
            slice_type,
            anon_struct_type,
            ptr_const_type,
            ptr_mut_type,
            primitive_type_parser(),
            self_type,
            comptime_type,
            qualified_type_call,
            type_call,
            str_fixed_type,
            qualified_type,
            named_type,
        ))
        // Summarize the whole type grammar as a single "type" expectation, so a
        // type-position error reads `expected type, found ...` instead of
        // enumerating the raw first tokens (`'(' or '!' or '['`). (RUE-19)
        .labelled("type")
    })
}

/// Parser for parameter mode: comptime, inout, or borrow
fn param_mode_parser<'src, I>() -> impl Parser<'src, I, ParamMode, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        just(TokenKind::Comptime).to(ParamMode::Comptime),
        just(TokenKind::Inout).to(ParamMode::Inout),
        just(TokenKind::Borrow).to(ParamMode::Borrow),
    ))
}

/// Parser for function parameters: [comptime|inout|borrow] name: type
///
/// A parameter takes at most one mode keyword. Repeated (`comptime comptime
/// T`) or conflicting (`comptime inout x`) modifiers used to be silently
/// accepted — the duplicate landed in a vestigial `is_comptime` flag next to
/// `ParamMode` — and are now rejected with a targeted error. (RUE-133)
fn param_parser<'src, I>() -> impl Parser<'src, I, Param, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    param_mode_parser()
        .map_with(|mode, e| (mode, e.span()))
        .repeated()
        .collect::<Vec<_>>()
        .try_map(|modes, _span| match modes.as_slice() {
            [] => Ok(ParamMode::Normal),
            [(mode, _)] => Ok(*mode),
            [(first, _), (second, second_span), ..] => {
                let msg = if first == second {
                    format!("duplicate parameter modifier '{}'", first.keyword())
                } else {
                    format!(
                        "conflicting parameter modifiers '{}' and '{}'; a parameter takes at most one of 'comptime', 'inout', or 'borrow'",
                        first.keyword(),
                        second.keyword()
                    )
                };
                Err(Rich::custom(*second_span, msg))
            }
        })
        .then(ident_parser())
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|((mode, name), ty), e| Param {
            mode,
            name,
            ty,
            span: span_from_extra(e),
        })
}

/// Parser for struct field declarations: name: type
fn field_decl_parser<'src, I>() -> impl Parser<'src, I, FieldDecl, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(name, ty), e| FieldDecl {
            name,
            ty,
            span: span_from_extra(e),
        })
}

/// Parser for comma-separated struct field declarations
fn field_decls_parser<'src, I>() -> impl Parser<'src, I, Vec<FieldDecl>, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    field_decl_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect()
}

/// Parser for comma-separated parameters
fn params_parser<'src, I>() -> impl Parser<'src, I, Vec<Param>, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    param_parser()
        .separated_by(just(TokenKind::Comma))
        .collect()
}

/// Parser for a single directive: @name or @name(arg1, arg2, ...)
fn directive_parser<'src, I>() -> impl Parser<'src, I, Directive, ParserExtras<'src>> + Clone
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
            span: span_from_extra(e),
        })
}

/// Parser for zero or more directives
fn directives_parser<'src, I>() -> impl Parser<'src, I, Directives, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Chumsky's collect() requires its Container trait, which SmallVec doesn't implement.
    // So we collect to Vec first, then convert. The overhead is minimal since most
    // items have 0-1 directives (Vec is cheap for empty/small collections).
    directive_parser()
        .repeated()
        .collect::<Vec<_>>()
        .map(|v| v.into_iter().collect())
}

/// Parser for argument mode: inout or borrow
fn arg_mode_parser<'src, I>() -> impl Parser<'src, I, ArgMode, ParserExtras<'src>> + Clone
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
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, CallArg, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    arg_mode_parser()
        .or_not()
        .then(expr)
        .map_with(|(mode, expr), e| CallArg {
            mode: mode.unwrap_or(ArgMode::Normal),
            expr,
            span: span_from_extra(e),
        })
}

/// Parser for comma-separated call arguments with optional inout
fn call_args_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, Vec<CallArg>, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    call_arg_parser(expr)
        .separated_by(just(TokenKind::Comma))
        .collect()
}

/// Parser for struct field initializers: name: expr
fn field_init_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, FieldInit, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(expr)
        .map_with(|(name, value), e| FieldInit {
            name,
            value: Box::new(value),
            span: span_from_extra(e),
        })
}

/// Parser for comma-separated field initializers
fn field_inits_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, Vec<FieldInit>, ParserExtras<'src>> + Clone
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
    let span = Span::with_file(left.span().file_id, left.span().start, right.span().end);
    Expr::Binary(BinaryExpr {
        left: Box::new(left),
        op,
        right: Box::new(right),
        span,
    })
}

/// Helper to create unary expression
fn make_unary(op: UnaryOp, operand: Expr, op_span: SimpleSpan) -> Expr {
    let span = Span::with_file(
        operand.span().file_id,
        offset_to_u32(op_span.start),
        operand.span().end,
    );
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
///
/// This ladder deliberately matches Rust's operator precedence
/// (spec rule 4.3a:13): shifts bind looser than `+`/`-` but tighter
/// than `&`, and the bitwise operators all bind tighter than
/// comparisons. So `a << b + c` is `a << (b + c)` and `a & b == c`
/// is `(a & b) == c`.
mod precedence {
    /// Logical OR: `||`
    pub const LOGICAL_OR: u16 = 1;
    /// Logical AND: `&&`
    pub const LOGICAL_AND: u16 = 2;
    /// Comparison: `==`, `!=`, `<`, `>`, `<=`, `>=`
    pub const COMPARISON: u16 = 3;
    /// Bitwise OR: `|`
    pub const BITWISE_OR: u16 = 4;
    /// Bitwise XOR: `^`
    pub const BITWISE_XOR: u16 = 5;
    /// Bitwise AND: `&`
    pub const BITWISE_AND: u16 = 6;
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
fn expr_parser<'src, I>() -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(|expr| {
        // Standalone block-like-statement parser (if/match/while/loop/block),
        // threaded into the atom and block parsers so that a block-like
        // expression in statement position is a complete statement (RUE-210).
        let block_like = block_like_head_parser(expr.clone());

        // Atom parser - primary expressions
        let atom = atom_parser(expr.clone(), block_like);

        // Rust-refugee diagnostic: `expr as T` is not Rue syntax (`as` is an
        // ordinary identifier, so `5 as i64` used to die with a generic
        // "expected semicolon" error). An expression directly followed by the
        // identifier `as` is never valid Rue, so consume it and report a
        // targeted error pointing at the `as`. (RUE-133)
        let as_misuse = select! { TokenKind::Ident(name) => name }
            .map_with(|name, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
                (name, e.state().syms.as_kw, e.span())
            })
            .filter(|(name, as_kw, _)| name == as_kw)
            .map(|(_, _, span)| span);

        // Build Pratt parser with precedence levels (see `precedence` module)
        let pratt = atom.pratt((
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
        ));

        pratt
            .then(as_misuse.or_not())
            .try_map(|(parsed, as_span), _span| match as_span {
                None => Ok(parsed),
                Some(span) => Err(Rich::custom(
                    span,
                    "Rue has no 'as' cast operator; use '@intCast(value)' (the target type comes \
                     from context) or give the target type on the binding: 'let x: i64 = value'",
                )),
            })
    })
}

/// Parser for patterns in match arms
fn pattern_parser<'src, I>() -> impl Parser<'src, I, Pattern, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Wildcard pattern: _
    let wildcard =
        just(TokenKind::Underscore).map_with(|_, e| Pattern::Wildcard(span_from_extra(e)));

    // Integer literal pattern (positive or zero)
    let int_pat = select! {
        TokenKind::Int(n) = e => Pattern::Int(IntLit {
            value: n,
            span: span_from_extra(e),
        }),
    };

    // Negative integer literal pattern: - followed by integer
    let neg_int_pat = just(TokenKind::Minus)
        .then(select! { TokenKind::Int(n) => n })
        .map_with(|(_, n), e| {
            Pattern::NegInt(NegIntLit {
                value: n,
                span: span_from_extra(e),
            })
        });

    // Boolean literal patterns
    let bool_true = select! {
        TokenKind::True = e => Pattern::Bool(BoolLit {
            value: true,
            span: span_from_extra(e),
        }),
    };

    let bool_false = select! {
        TokenKind::False = e => Pattern::Bool(BoolLit {
            value: false,
            span: span_from_extra(e),
        }),
    };

    // Optional payload bindings for a tuple-variant pattern: `(a, b, ...)`
    // (RUE-221). At least one binding required inside the parens.
    let pattern_bindings = ident_parser()
        .separated_by(just(TokenKind::Comma))
        .at_least(1)
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .or_not()
        .map(|opt| opt.unwrap_or_default());

    // Simple enum path pattern: Enum.Variant (RUE-488). `.` is the sole
    // member-access spelling; `::` was removed.
    let simple_dot_path_pat = ident_parser()
        .then_ignore(just(TokenKind::Dot))
        .then(ident_parser())
        .then(pattern_bindings.clone())
        .map_with(|((type_name, variant), bindings), e| {
            Pattern::Path(PathPattern {
                base: None,
                type_name,
                variant,
                bindings,
                span: span_from_extra(e),
            })
        });

    // Qualified enum path pattern:
    // module.Enum.Variant or module.sub.Enum.Variant (RUE-488). The final two
    // identifiers are the enum type and variant; any earlier identifiers form
    // the module/member base expression.
    let qualified_dot_path_pat = ident_parser()
        .then(
            just(TokenKind::Dot)
                .ignore_then(ident_parser())
                .repeated()
                .at_least(2)
                .collect::<Vec<_>>(),
        )
        .then(pattern_bindings)
        .map_with(|((first, mut rest), bindings), e| {
            let variant = rest.pop().expect("at_least(2) guarantees variant");
            let type_name = rest.pop().expect("at_least(2) guarantees type name");

            let base_expr = if rest.is_empty() {
                Expr::Ident(first.clone())
            } else {
                let mut base = Expr::Ident(first.clone());
                for field in rest {
                    let span = base.span().extend_to(field.span.end);
                    base = Expr::Field(FieldExpr {
                        base: Box::new(base),
                        field,
                        span,
                    });
                }
                base
            };

            Pattern::Path(PathPattern {
                base: Some(Box::new(base_expr)),
                type_name,
                variant,
                bindings,
                span: span_from_extra(e),
            })
        });

    choice((
        wildcard,
        neg_int_pat,
        int_pat,
        bool_true,
        bool_false,
        // The qualified form (`module.Enum.Variant`, ≥2 dots) must be tried
        // before the simple one (`Enum.Variant`, exactly 1 dot); otherwise the
        // simple parser would greedily match the first two identifiers of a
        // qualified pattern and leave the rest unconsumed.
        qualified_dot_path_pat,
        simple_dot_path_pat,
    ))
}

/// Parser for a single match arm: pattern => expr
fn match_arm_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, MatchArm, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    pattern_parser()
        .then_ignore(just(TokenKind::FatArrow))
        .then(expr)
        .map_with(|(pattern, body), e| MatchArm {
            pattern,
            body: Box::new(body),
            span: span_from_extra(e),
        })
}

/// Parser for literal expressions: integers, strings, booleans, and unit
fn literal_parser<'src, I>() -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Integer literal
    let int_lit = select! {
        TokenKind::Int(n) = e => Expr::Int(IntLit {
            value: n,
            span: span_from_extra(e),
        }),
    };

    // String literal
    let string_lit = select! {
        TokenKind::String(s) = e => Expr::String(StringLit {
            value: s,
            span: span_from_extra(e),
        }),
    };

    // Boolean literals
    let bool_true = select! {
        TokenKind::True = e => Expr::Bool(BoolLit {
            value: true,
            span: span_from_extra(e),
        }),
    };

    let bool_false = select! {
        TokenKind::False = e => Expr::Bool(BoolLit {
            value: false,
            span: span_from_extra(e),
        }),
    };

    // Unit literal: ()
    let unit_lit = just(TokenKind::LParen)
        .then(just(TokenKind::RParen))
        .map_with(|_, e| {
            Expr::Unit(UnitLit {
                span: span_from_extra(e),
            })
        });

    choice((int_lit, string_lit, bool_true, bool_false, unit_lit))
}

fn empty_body_from_reclaimed_struct_lit(lit: &StructLitExpr) -> BlockExpr {
    let span = Span::with_file(lit.span.file_id, lit.name.span.end, lit.span.end);
    BlockExpr {
        statements: Vec::new(),
        expr: Box::new(Expr::Unit(UnitLit { span })),
        span,
    }
}

fn reclaim_empty_struct_lit_head(lit: StructLitExpr) -> Option<Expr> {
    if !lit.fields.is_empty() {
        return None;
    }

    Some(match lit.base {
        None => Expr::Ident(lit.name),
        Some(base) => {
            let span = base.span().extend_to(lit.name.span.end);
            Expr::Field(FieldExpr {
                base,
                field: lit.name,
                span,
            })
        }
    })
}

/// Parser for control flow expressions: break, continue, return, if, while, loop, match
///
/// `block_like` is the standalone block-like-statement parser threaded down to
/// the block bodies (`if`/`while`/`loop`/`match` arms) so those bodies apply
/// the block-like statement-boundary rule (RUE-210).
fn control_flow_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
    block_like: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Break: break <expr>? (a value operand parses, but sema always rejects
    // it - break does not carry a value; parsing it gives a better diagnostic
    // than treating the operand as a separate, unreachable statement)
    //
    // The operand is refused when it starts with `{` so that a bare `break`
    // used as a loop/if condition does not swallow the following block as its
    // value: in `while break {}` the `{}` is the loop body, not a block-valued
    // operand of `break`. The `and_is(LBrace.not())` non-consuming guard makes
    // the optional operand decline a leading brace, leaving it for the
    // loop/if body parser (RUE-209). `break (expr)` is unaffected.
    let break_expr = just(TokenKind::Break)
        .ignore_then(expr.clone().and_is(just(TokenKind::LBrace).not()).or_not())
        .map_with(|value, e| {
            Expr::Break(BreakExpr {
                value: value.map(Box::new),
                span: span_from_extra(e),
            })
        });

    // Continue
    let continue_expr = select! {
        TokenKind::Continue = e => Expr::Continue(ContinueExpr { span: span_from_extra(e) }),
    };

    // Return expression: return <expr>? (expression is optional for unit-returning functions)
    //
    // As with `break` above, the optional operand declines a leading `{` so a
    // bare `return` used as a loop/if condition does not swallow the following
    // block as its value operand (RUE-209). `return (expr)` is unaffected.
    let return_expr = just(TokenKind::Return)
        .ignore_then(expr.clone().and_is(just(TokenKind::LBrace).not()).or_not())
        .map_with(|value, e| {
            Expr::Return(ReturnExpr {
                value: value.map(Box::new),
                span: span_from_extra(e),
            })
        });

    // If expression - defined with recursive reference to allow `else if` chains
    //
    // The then block is `.or_not()` because of a brace ambiguity: in `if v {}`
    // the expression parser greedily parses `v {}` as a zero-field struct
    // literal, swallowing the braces meant to be the empty then block. Per spec
    // 4.6:13 a struct literal may not appear unparenthesized as the condition,
    // so the `try_map_with` below reinterprets that case as an empty block and
    // rejects any other bare struct-literal condition with a targeted
    // diagnostic (RUE-373, mirroring RUE-169 for `match`).
    let if_expr = recursive(|if_expr_rec| {
        just(TokenKind::If)
            .ignore_then(expr.clone())
            .then(block_parser(expr.clone(), block_like.clone()).or_not())
            .then(
                just(TokenKind::Else)
                    .ignore_then(choice((
                        // else if: wrap the nested if in a synthetic block
                        if_expr_rec.map_with(|nested_if, e| {
                            let span = span_from_extra(e);
                            BlockExpr {
                                statements: Vec::new(),
                                expr: Box::new(nested_if),
                                span,
                            }
                        }),
                        // else { ... }: parse a regular block
                        block_parser(expr.clone(), block_like.clone()),
                    )))
                    .or_not(),
            )
            .try_map_with(|((cond, then_block), else_block), e| {
                let span = span_from_extra(e);
                match (cond, then_block) {
                    (Expr::StructLit(lit), None) => {
                        let then_block = empty_body_from_reclaimed_struct_lit(&lit);
                        let Some(cond) = reclaim_empty_struct_lit_head(lit) else {
                            return Err(Rich::custom(
                                e.span(),
                                "struct literals are not allowed as a bare if condition; \
                                 wrap the condition in parentheses",
                            ));
                        };

                        Ok(Expr::If(IfExpr {
                            cond: Box::new(cond),
                            then_block,
                            else_block,
                            span,
                        }))
                    }
                    (Expr::StructLit(_), _) => Err(Rich::custom(
                        e.span(),
                        "struct literals are not allowed as a bare if condition; \
                         wrap the condition in parentheses",
                    )),
                    (cond, Some(then_block)) => Ok(Expr::If(IfExpr {
                        cond: Box::new(cond),
                        then_block,
                        else_block,
                        span,
                    })),
                    (_, None) => Err(Rich::custom(
                        e.span(),
                        "expected '{' and then block after the if condition",
                    )),
                }
            })
    })
    .boxed();

    // While expression
    //
    // As with `if`, the body is optional at first so `while v {}` can reclaim
    // the zero-field struct literal's braces as the empty loop body.
    let while_expr = just(TokenKind::While)
        .ignore_then(expr.clone())
        .then(block_parser(expr.clone(), block_like.clone()).or_not())
        .try_map_with(|(cond, body), e| {
            let span = span_from_extra(e);
            match (cond, body) {
                (Expr::StructLit(lit), None) => {
                    let body = empty_body_from_reclaimed_struct_lit(&lit);
                    let Some(cond) = reclaim_empty_struct_lit_head(lit) else {
                        return Err(Rich::custom(
                            e.span(),
                            "struct literals are not allowed as a bare while condition; \
                             wrap the condition in parentheses",
                        ));
                    };

                    Ok(Expr::While(WhileExpr {
                        cond: Box::new(cond),
                        body,
                        span,
                    }))
                }
                (Expr::StructLit(_), _) => Err(Rich::custom(
                    e.span(),
                    "struct literals are not allowed as a bare while condition; \
                     wrap the condition in parentheses",
                )),
                (cond, Some(body)) => Ok(Expr::While(WhileExpr {
                    cond: Box::new(cond),
                    body,
                    span,
                })),
                (_, None) => Err(Rich::custom(
                    e.span(),
                    "expected '{' and body block after the while condition",
                )),
            }
        })
        .boxed();

    // Loop expression (infinite loop)
    let loop_expr = just(TokenKind::Loop)
        .ignore_then(block_parser(expr.clone(), block_like.clone()))
        .map_with(|body, e| {
            Expr::Loop(LoopExpr {
                body,
                span: span_from_extra(e),
            })
        })
        .boxed();

    // For expression (RUE-220): `for <binder> in <iterable> { body }`.
    //
    // The iterable is a full expression followed by the body block. Like `if`
    // and `while`, the body is optional at first so `for x in v {}` can reclaim
    // the zero-field struct literal's braces as the empty loop body.
    let for_expr = just(TokenKind::For)
        .ignore_then(let_pattern_parser())
        .then_ignore(just(TokenKind::In))
        .then(expr.clone())
        .then(block_parser(expr.clone(), block_like.clone()).or_not())
        .try_map_with(|((binder, iterable), body), e| {
            let span = span_from_extra(e);
            match (iterable, body) {
                (Expr::StructLit(lit), None) => {
                    let body = empty_body_from_reclaimed_struct_lit(&lit);
                    let Some(iterable) = reclaim_empty_struct_lit_head(lit) else {
                        return Err(Rich::custom(
                            e.span(),
                            "struct literals are not allowed as a bare for iterable; \
                             wrap the iterable in parentheses",
                        ));
                    };

                    Ok(Expr::For(ForExpr {
                        binder,
                        iterable: Box::new(iterable),
                        body,
                        span,
                    }))
                }
                (Expr::StructLit(_), _) => Err(Rich::custom(
                    e.span(),
                    "struct literals are not allowed as a bare for iterable; \
                     wrap the iterable in parentheses",
                )),
                (iterable, Some(body)) => Ok(Expr::For(ForExpr {
                    binder,
                    iterable: Box::new(iterable),
                    body,
                    span,
                })),
                (_, None) => Err(Rich::custom(
                    e.span(),
                    "expected '{' and body block after the for iterable",
                )),
            }
        })
        .boxed();

    // Match expression: match scrutinee { pattern => expr, ... }
    //
    // The arm block is `.or_not()` because of a brace ambiguity: in
    // `match v {}` the expression parser greedily parses `v {}` as a
    // zero-field struct literal, swallowing the braces that were meant to be
    // the (empty) arm list. Per spec 4.7:28 a struct literal may not appear
    // unparenthesized as the scrutinee, so the `try_map_with` below
    // reinterprets that case as an empty arm list and rejects any other bare
    // struct-literal scrutinee with a targeted diagnostic (RUE-169).
    let match_expr = just(TokenKind::Match)
        .ignore_then(expr.clone())
        .then(
            match_arm_parser(expr)
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
                .or_not(),
        )
        .try_map_with(|(scrutinee, arms), e| {
            let span = {
                let file_id = e.state().0.file_id;
                to_rue_span_with_file(e.span(), file_id)
            };
            match (scrutinee, arms) {
                // `match v {}`: reclaim the struct literal's braces as the
                // match's empty arm list; the scrutinee is the bare identifier.
                (Expr::StructLit(lit), None) if lit.base.is_none() && lit.fields.is_empty() => {
                    Ok(Expr::Match(MatchExpr {
                        scrutinee: Box::new(Expr::Ident(lit.name)),
                        arms: Vec::new(),
                        span,
                    }))
                }
                (Expr::StructLit(_), _) => Err(Rich::custom(
                    e.span(),
                    "struct literals are not allowed as a bare match scrutinee; \
                     wrap the scrutinee in parentheses",
                )),
                (scrutinee, Some(arms)) => Ok(Expr::Match(MatchExpr {
                    scrutinee: Box::new(scrutinee),
                    arms,
                    span,
                })),
                (_, None) => Err(Rich::custom(
                    e.span(),
                    "expected '{' and match arms after the match scrutinee",
                )),
            }
        })
        .boxed();

    choice((
        break_expr,
        continue_expr,
        return_expr,
        if_expr,
        while_expr,
        loop_expr,
        for_expr,
        match_expr,
    ))
}

/// What can follow an identifier: call args, struct fields, or nothing.
///
/// Enum-variant paths (`Enum.Variant`) and associated-function calls
/// (`Type.function()`) are spelled with `.` (RUE-488) and parse as ordinary
/// field-access / method-call suffixes in `with_suffix_parser`; sema
/// reinterprets them when the base names a type. So no path suffix appears here.
#[derive(Clone)]
enum IdentSuffix {
    Call(Vec<CallArg>),
    StructLit(Vec<FieldInit>),
    None,
}

/// Parser for identifier-based expressions: identifiers, function calls, struct literals, and paths
fn call_and_access_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
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
            ))
            .or_not()
            .map(|opt| opt.unwrap_or(IdentSuffix::None)),
        )
        .map_with(|(name, suffix), e| match suffix {
            IdentSuffix::Call(args) => Expr::Call(CallExpr {
                name,
                args,
                span: span_from_extra(e),
            }),
            IdentSuffix::StructLit(fields) => Expr::StructLit(StructLitExpr {
                base: None, // No module prefix for simple `StructName { ... }`
                name,
                fields,
                span: span_from_extra(e),
            }),
            IdentSuffix::None => Expr::Ident(name),
        })
}

/// Suffix for field access (.field), method call (.method(args)), indexing ([expr]),
/// qualified struct literals (.Type { ... }), and qualified paths (.Enum::Variant)
#[derive(Clone)]
enum Suffix {
    /// Simple field access: .field
    Field(Ident),
    /// Method call with method name, arguments, and closing paren position
    MethodCall(Ident, Vec<CallArg>, u32),
    /// Index expression with the inner expression and closing bracket position
    Index(Expr, u32),
    /// Qualified struct literal: .StructName { fields }
    /// (Disambiguated from field access + block by the `{ }` / `{ ident :`
    /// lookahead in `with_suffix_parser`.)
    QualifiedStructLit(Ident, Vec<FieldInit>, u32),
    /// Try/`?` propagation: the `?` postfix operator, carrying the position
    /// just past the `?` token so the resulting span covers `operand?`.
    Try(u32),
}

/// Wraps a primary expression parser with field access, method call, and indexing suffixes
fn with_suffix_parser<'src, I>(
    primary: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
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

    // Qualified struct literal: .StructName { field: value, ... }
    // We need to distinguish between:
    //   - `module.Point { x: 1 }` - qualified struct literal
    //   - `obj.field { 1 }` - field access followed by a block expression
    //
    // The key difference is that struct literals have `{ ident: value, ... }` while
    // block expressions have `{ expr }`. We use a lookahead to require that `{` is
    // followed by `}` (empty struct) or `ident :` (non-empty struct) to confirm it's
    // a struct literal, not field access followed by a block.
    //
    // Lookahead check: succeeds (without consuming) if `{ }` or `{ ident : `
    let struct_lit_lookahead = just(TokenKind::LBrace)
        .then(
            choice((
                // Empty struct: { }
                just(TokenKind::RBrace).ignored(),
                // Non-empty struct: { ident : ...
                select! { TokenKind::Ident(_) => () }
                    .then_ignore(just(TokenKind::Colon))
                    .ignored(),
            ))
            // We're just checking the pattern exists, not consuming
            .rewind(),
        )
        .rewind();

    // NOTE: .boxed() is required here to shorten the monomorphized type name.
    // The lookahead creates deeply nested generics that exceed macOS linker limits.
    let qualified_struct_lit_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        // Use lookahead to confirm this is a struct literal, not field + block
        .then_ignore(struct_lit_lookahead)
        .then(
            field_inits_parser(expr.clone())
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|(name, fields), e| {
            Suffix::QualifiedStructLit(name, fields, offset_to_u32(e.span().end))
        })
        .boxed();

    // Field access: .ident (but NOT followed by '(' or a struct-literal
    // pattern). Enum-variant paths (`base.Enum.Variant`) and associated-function
    // calls (`base.Type.func()`) are also spelled with `.` (RUE-488); they parse
    // as field-access / method-call chains here and sema reinterprets them when
    // the base names a type.
    // The qualified_struct_lit_suffix is tried first and uses lookahead, so field_suffix
    // only matches when we're certain it's field access, not a qualified struct literal.
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .then_ignore(none_of([TokenKind::LParen]).rewind())
        .map(Suffix::Field);

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|index, e| Suffix::Index(index, offset_to_u32(e.span().end)));

    // Order matters: more specific patterns must come before less specific ones
    // - method_call_suffix before field_suffix (both start with .ident)
    // - qualified_struct_lit_suffix before field_suffix (uses lookahead for { ident: } pattern)
    //
    // NOTE: .boxed() is required here to shorten the monomorphized type name.
    // Without it, macOS's ld64 linker fails with "symbol name too long" because
    // the choice creates extremely long generic type names.
    primary.foldl(
        choice((
            method_call_suffix,
            qualified_struct_lit_suffix,
            field_suffix,
            index_suffix,
            question_suffix_parser(),
        ))
        .boxed()
        .repeated(),
        |base, suffix| match suffix {
            Suffix::Field(field) => {
                // Extend the base span to include the field, preserving file_id
                let span = base.span().extend_to(field.span.end);
                Expr::Field(FieldExpr {
                    base: Box::new(base),
                    field,
                    span,
                })
            }
            Suffix::MethodCall(method, args, end) => {
                // Extend the base span to the end of the call, preserving file_id
                let span = base.span().extend_to(end);
                Expr::MethodCall(MethodCallExpr {
                    receiver: Box::new(base),
                    method,
                    args,
                    span,
                })
            }
            Suffix::Index(index, end) => {
                // Extend the base span to the end of the index, preserving file_id
                let span = base.span().extend_to(end);
                Expr::Index(IndexExpr {
                    base: Box::new(base),
                    index: Box::new(index),
                    span,
                })
            }
            Suffix::Try(end) => {
                // Extend the base span through the `?`, preserving file_id.
                let span = base.span().extend_to(end);
                Expr::Try(TryExpr {
                    operand: Box::new(base),
                    span,
                })
            }
            Suffix::QualifiedStructLit(name, fields, end) => {
                // module.StructName { ... } → StructLitExpr with base
                let span = base.span().extend_to(end);
                Expr::StructLit(StructLitExpr {
                    base: Some(Box::new(base)),
                    name,
                    fields,
                    span,
                })
            }
        },
    )
}

/// Atom parser - primary expressions (literals, identifiers, parens, blocks, control flow)
fn atom_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
    block_like: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Self expression (in method bodies)
    let self_expr = select! {
        TokenKind::SelfValue = e => Expr::SelfExpr(SelfExpr { span: span_from_extra(e) }),
    };

    // Parenthesized expression
    let paren_expr = expr
        .clone()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .map_with(|inner, e| {
            Expr::Paren(ParenExpr {
                inner: Box::new(inner),
                span: span_from_extra(e),
            })
        });

    // Block expression
    let block_expr = block_parser(expr.clone(), block_like.clone()).map(Expr::Block);

    // Comptime block expression: comptime { expr }
    let comptime_expr = just(TokenKind::Comptime)
        .ignore_then(block_parser(expr.clone(), block_like.clone()).map(Expr::Block))
        .map_with(|inner_expr, e| {
            Expr::Comptime(ComptimeBlockExpr {
                expr: Box::new(inner_expr),
                span: span_from_extra(e),
            })
        });

    // Checked block expression: checked { expr }
    // Unchecked operations are only allowed inside checked blocks
    let checked_expr = just(TokenKind::Checked)
        .ignore_then(block_parser(expr.clone(), block_like.clone()).map(Expr::Block))
        .map_with(|inner_expr, e| {
            Expr::Checked(CheckedBlockExpr {
                expr: Box::new(inner_expr),
                span: span_from_extra(e),
            })
        });

    // Intrinsic argument: can be either a type or an expression
    // We parse as type only for unambiguous type syntax (primitives, (), !, [T;N])
    // Bare identifiers are parsed as expressions since they could be variables
    let unambiguous_type = {
        // Unit type: ()
        let unit_type = just(TokenKind::LParen)
            .then(just(TokenKind::RParen))
            .map_with(|_, e| IntrinsicArg::Type(TypeExpr::Unit(span_from_extra(e))));

        // Never type: ! — but only when the `!` is the entire argument (the
        // next token is `,` or `)`). A `!` followed by anything else is the
        // logical-not operator (e.g. `@dbg(!flag)`), which must fall through
        // to the expression branch; committing to the never-type parse here
        // used to reject those arguments (RUE-150). The lookahead is
        // non-consuming (`rewind`) so the delimiter is still parsed normally.
        let never_type = just(TokenKind::Bang)
            .then_ignore(one_of([TokenKind::Comma, TokenKind::RParen]).rewind())
            .map_with(|_, e| IntrinsicArg::Type(TypeExpr::Never(span_from_extra(e))));

        // Array type: [T; N] where N is a literal, a named length (RUE-16), or
        // a comptime-evaluable call `f(args)` (RUE-309).
        let array_length = recursive(|array_length| {
            let call = ident_parser()
                .then(
                    array_length
                        .separated_by(just(TokenKind::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
                )
                .map(|(name, args)| ArrayLength::Call { name, args });
            choice((
                select! { TokenKind::Int(n) => ArrayLength::Literal(n as u64) },
                call,
                ident_parser().map(ArrayLength::Named),
            ))
        });
        let array_type = just(TokenKind::LBracket)
            .ignore_then(type_parser())
            .then_ignore(just(TokenKind::Semi))
            .then(array_length)
            .then_ignore(just(TokenKind::RBracket))
            .map_with(|(element, length), e| {
                IntrinsicArg::Type(TypeExpr::Array {
                    element: Box::new(element),
                    length,
                    span: span_from_extra(e),
                })
            });

        // Primitive type keywords (these can't be variable names)
        let primitive_type = primitive_type_parser().map(IntrinsicArg::Type);

        choice((unit_type, never_type, array_type, primitive_type))
    };

    // Try unambiguous type syntax first, then fall back to expression
    let intrinsic_arg = unambiguous_type.or(expr.clone().map(IntrinsicArg::Expr));

    // The intrinsic name is normally an identifier, but `@drop` (RUE-187)
    // names the intentional-destroy intrinsic with the `drop` KEYWORD (also
    // used by `drop fn`). The lexer emits a dedicated `Drop` token for it, so
    // accept that token here and map it back to the interned "drop" symbol.
    let drop_intrinsic_name =
        just(TokenKind::Drop).map_with(|_, e: &mut MapExtra<'src, '_, I, ParserExtras<'src>>| {
            Ident {
                name: e.state().0.syms.drop_kw,
                span: span_from_extra(e),
            }
        });
    let intrinsic_name = ident_parser().or(drop_intrinsic_name);

    // Intrinsic call: @name(args) or @import(args)
    // The lexer tokenizes @import specially, so we need to handle both cases
    let intrinsic_call = just(TokenKind::At)
        .ignore_then(intrinsic_name)
        .then(
            intrinsic_arg
                .clone()
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
        )
        .map_with(|(name, args), e| {
            let syms = e.state().0.syms;
            (name, args, span_from_extra(e), syms)
        })
        .try_map(|(name, args, expr_span, syms), span| {
            if name.name == syms.allow_directive || name.name == syms.copy_directive {
                return Err(Rich::custom(span, "directive must precede a statement"));
            }
            Ok(Expr::IntrinsicCall(IntrinsicCallExpr {
                name,
                args,
                span: expr_span,
            }))
        });

    // @import(args) - lexer tokenizes @import as a single token with interned "import" Spur
    let import_call = select! {
        TokenKind::AtImport(import_spur) = e => (import_spur, span_from_extra(e)),
    }
    .then(
        intrinsic_arg
            .separated_by(just(TokenKind::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)),
    )
    .map_with(|((import_spur, import_span), args), e| {
        Expr::IntrinsicCall(IntrinsicCallExpr {
            name: Ident {
                name: import_spur,
                span: import_span,
            },
            args,
            span: span_from_extra(e),
        })
    });

    // Combined intrinsic parser: try @import first, then general @name pattern
    let any_intrinsic_call = import_call.or(intrinsic_call);

    // Array literal, two forms (RUE-235):
    //   list form:   [expr, expr, ...]
    //   repeat form: [value ; count]   where count is a compile-time constant
    //                                  (integer literal or named const/comptime)
    // The repeat form is distinguished by the `;` following the first expr. Parse
    // the bracket pair and first expression once, then branch on the separator;
    // trying `[expr ; count]` as a full alternative before `[expr, ...]` reparses
    // deeply-nested list elements exponentially when there is no `;` (RUE-374).
    let array_repeat_count = choice((
        select! { TokenKind::Int(n) => ArrayLength::Literal(n as u64) },
        ident_parser().map(ArrayLength::Named),
    ));
    let array_tail = choice((
        just(TokenKind::Semi)
            .ignore_then(array_repeat_count)
            .map(|count| (Vec::new(), Some(count))),
        just(TokenKind::Comma)
            .ignore_then(
                expr.clone()
                    .separated_by(just(TokenKind::Comma))
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .map(|rest| (rest, None)),
        just(TokenKind::RBracket)
            .rewind()
            .map(|_| (Vec::new(), None)),
    ));
    let non_empty_array = expr
        .clone()
        .then(array_tail)
        .map(|(first, (mut rest, repeat))| {
            if repeat.is_some() {
                (vec![first], repeat)
            } else {
                let mut elements = vec![first];
                elements.append(&mut rest);
                (elements, None)
            }
        });
    let empty_array = just(TokenKind::RBracket)
        .rewind()
        .map(|_| (Vec::new(), None));
    let array_lit = non_empty_array
        .or(empty_array)
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|(elements, repeat), e| {
            Expr::ArrayLit(ArrayLitExpr {
                elements,
                repeat,
                span: span_from_extra(e),
            })
        });

    // Type literal expression: i32, bool, etc. used as values
    // This enables generic function calls like identity(i32, 42)
    let type_lit_expr = primitive_type_parser().map_with(|type_expr, e| {
        Expr::TypeLit(TypeLitExpr {
            type_expr,
            span: span_from_extra(e),
        })
    });

    // Anonymous struct type as expression: struct { field: Type, ... fn method(...) { ... } ... }
    // This enables comptime type construction like:
    //   fn Pair(comptime T: type) -> type { struct { first: T, second: T } }
    // With methods (Zig-style):
    //   fn Vec(comptime T: type) -> type { struct { ptr: u64, fn push(self, item: T) { ... } } }
    let anon_struct_field = ident_parser()
        .then_ignore(just(TokenKind::Colon))
        .then(type_parser())
        .map_with(|(name, field_ty), e| AnonStructField {
            name,
            ty: field_ty,
            span: span_from_extra(e),
        });

    // Parse a member of an anonymous struct body: either a `drop fn(self)`
    // destructor (RUE-312) or an ordinary method. Both produce a `Method`; the
    // destructor is carried under the reserved `__drop` name. `drop` is tried
    // first because it is a distinct keyword token (an ordinary method always
    // starts with directives or `fn`).
    let anon_struct_member = choice((
        anon_struct_drop_fn_parser(expr.clone()),
        anon_struct_method_parser(expr.clone()),
    ));

    let anon_struct_type_expr = just(TokenKind::Struct)
        .ignore_then(just(TokenKind::LBrace))
        .ignore_then(
            // Parse fields first (comma-separated, trailing comma allowed)
            anon_struct_field
                .separated_by(just(TokenKind::Comma))
                .allow_trailing()
                .collect::<Vec<_>>(),
        )
        .then(
            // Then parse methods and an optional `drop fn` (not comma-separated,
            // each ends with })
            anon_struct_member.repeated().collect::<Vec<_>>(),
        )
        .then_ignore(just(TokenKind::RBrace))
        .map_with(|(fields, methods), e| {
            let span = span_from_extra(e);
            Expr::TypeLit(TypeLitExpr {
                type_expr: TypeExpr::AnonymousStruct {
                    fields,
                    methods,
                    span,
                },
                span,
            })
        });

    // Anonymous enum type as expression: enum { Variant1, Variant2(T), ... }
    // The enum analog of anon_struct_type_expr; enables comptime generic sum
    // types like `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
    // (ADR-0038, RUE-6 phase 2).
    let anon_enum_type_expr = just(TokenKind::Enum)
        .ignore_then(
            enum_variants_parser().delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|variants, e| {
            let span = span_from_extra(e);
            Expr::TypeLit(TypeLitExpr {
                type_expr: TypeExpr::AnonymousEnum { variants, span },
                span,
            })
        });

    // Self type expression: Self { field: value } (struct literal with Self as type)
    // This enables constructing instances of anonymous struct types from methods
    let self_type_expr = just(TokenKind::SelfType)
        .ignore_then(
            field_inits_parser(expr.clone())
                .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)),
        )
        .map_with(|fields, e| {
            let span = span_from_extra(e);
            Expr::StructLit(StructLitExpr {
                base: None,
                name: Ident {
                    name: e.state().syms.self_type,
                    span,
                },
                fields,
                span,
            })
        });

    // Primary expression (before field access and indexing)
    // Note: literal_parser() includes unit_lit which must come before paren_expr
    // so () is parsed as unit, not empty parens
    // Note: self_expr must come before call_and_access_parser since self is a keyword
    // Note: self_type_expr must come before call_and_access_parser since Self is a keyword
    // Note: comptime_expr and checked_expr must come before block_expr since they start with keywords
    // Note: type_lit_expr must come before call_and_access_parser since type names are keywords
    // Note: anon_struct_type_expr must come before call_and_access_parser since struct is a keyword
    let primary = choice((
        literal_parser(),
        control_flow_parser(expr.clone(), block_like.clone()),
        self_expr,
        self_type_expr,
        any_intrinsic_call,
        array_lit,
        anon_struct_type_expr,
        anon_enum_type_expr,
        type_lit_expr,
        call_and_access_parser(expr.clone()),
        paren_expr,
        comptime_expr,
        checked_expr,
        block_expr,
    ))
    // When every atom alternative fails at this position none of them can
    // enumerate its accepted token set (the literal/`self` branches are bare
    // `select!` filters), so chumsky would otherwise report the useless
    // catch-all "expected something else". Describe the syntactic context
    // instead: at an atom position what is wanted is an expression. (RUE-19)
    .labelled("expression");

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
fn let_pattern_parser<'src, I>() -> impl Parser<'src, I, LetPattern, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let wildcard =
        just(TokenKind::Underscore).map_with(|_, e| LetPattern::Wildcard(span_from_extra(e)));
    let ident = ident_parser().map(LetPattern::Ident);
    ident.or(wildcard)
}

/// Parser for let statements: [@directive]* let [mut] pattern [: type] = expr;
fn let_statement_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, Statement, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let plain_let = just(TokenKind::Let).to(smallvec::SmallVec::new());
    let directed_let = directive_parser()
        .repeated()
        .at_least(1)
        .collect::<Vec<_>>()
        .then(just(TokenKind::Let).or_not())
        .try_map(|(directives, let_token), span| {
            if let_token.is_none() {
                return Err(Rich::custom(span, "directive must precede a statement"));
            }
            Ok(directives.into_iter().collect())
        });

    choice((directed_let, plain_let))
        .then(just(TokenKind::Mut).or_not().map(|m| m.is_some()))
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
                span: span_from_extra(e),
            })
        })
}

/// Suffix for assignment targets: either .field or [index]. The index form
/// carries the offset of its closing `]` so the resulting place span covers
/// the whole `base[index]`, not just up to the end of `index` (mirrors the
/// RHS index suffix, `Suffix::Index`).
#[derive(Clone)]
enum AssignSuffix {
    Field(Ident),
    Index(Expr, u32),
}

/// Parser for assignment target: variable, field access, or index access
/// Parses: name or name.field or name[idx] or name.field[idx].field...
fn assign_target_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, AssignTarget, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let field_suffix = just(TokenKind::Dot)
        .ignore_then(ident_parser())
        .map(AssignSuffix::Field);

    let index_suffix = expr
        .delimited_by(just(TokenKind::LBracket), just(TokenKind::RBracket))
        .map_with(|index, e| AssignSuffix::Index(index, offset_to_u32(e.span().end)));

    let suffix = choice((field_suffix, index_suffix));

    // Identifier base: `x`, `x.a[0].b`, ... (a bare `x` is a variable target).
    let ident_target = ident_parser()
        .then(suffix.clone().repeated().collect::<Vec<_>>())
        .map(|(base_ident, suffixes)| {
            if suffixes.is_empty() {
                // Simple variable: x
                AssignTarget::Var(base_ident.clone())
            } else {
                build_projected_assign_target(Expr::Ident(base_ident), suffixes)
            }
        });

    // `self` base: `self.a`, `self.a[0]`, ... — for mutating a field of an
    // `inout self` receiver (RUE-15). At least one projection is required: a
    // bare `self` is not an assignable place.
    let self_target = just(TokenKind::SelfValue)
        .map_with(|_, e| SelfExpr {
            span: span_from_extra(e),
        })
        .then(suffix.repeated().at_least(1).collect::<Vec<_>>())
        .map(|(self_expr, suffixes)| {
            build_projected_assign_target(Expr::SelfExpr(self_expr), suffixes)
        });

    choice((self_target, ident_target))
}

/// Build a field/index assignment target from a base expression and a
/// non-empty chain of field/index projections. The last projection determines
/// whether the target is a `Field` or `Index` place; earlier ones become
/// intermediate `Expr::Field`/`Expr::Index` accesses on the base.
fn build_projected_assign_target(base: Expr, suffixes: Vec<AssignSuffix>) -> AssignTarget {
    debug_assert!(!suffixes.is_empty());
    let mut base_expr = base;
    let mut suffixes = suffixes.into_iter().peekable();
    while let Some(suffix) = suffixes.next() {
        let is_last = suffixes.peek().is_none();
        if is_last {
            return match suffix {
                AssignSuffix::Field(field) => {
                    let span = base_expr.span().extend_to(field.span.end);
                    AssignTarget::Field(FieldExpr {
                        base: Box::new(base_expr),
                        field,
                        span,
                    })
                }
                AssignSuffix::Index(index, bracket_end) => {
                    let span = base_expr.span().extend_to(bracket_end);
                    AssignTarget::Index(IndexExpr {
                        base: Box::new(base_expr),
                        index: Box::new(index),
                        span,
                    })
                }
            };
        }
        match suffix {
            AssignSuffix::Field(field) => {
                let span = base_expr.span().extend_to(field.span.end);
                base_expr = Expr::Field(FieldExpr {
                    base: Box::new(base_expr),
                    field,
                    span,
                });
            }
            AssignSuffix::Index(index, bracket_end) => {
                let span = base_expr.span().extend_to(bracket_end);
                base_expr = Expr::Index(IndexExpr {
                    base: Box::new(base_expr),
                    index: Box::new(index),
                    span,
                });
            }
        }
    }
    unreachable!("suffixes is non-empty")
}

/// Parser for assignment statements: target = expr;
/// Supports variable (x = 5), field (point.x = 5), and index (arr[0] = 5) assignment
fn assign_statement_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone,
) -> impl Parser<'src, I, Statement, ParserExtras<'src>> + Clone
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
                span: span_from_extra(e),
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
            | Expr::For(_)
            | Expr::Break(_)
            | Expr::Continue(_)
            | Expr::Return(_)
            | Expr::Block(_)
    )
}

/// Returns true if the expression is "block-like": a control-flow form whose
/// syntax ends in a `}` block (`if`/`match`/`while`/`loop`) or a bare block.
///
/// In statement position, a block-like expression immediately followed by a
/// `-` is a complete statement, and the `-` starts a NEW statement as unary
/// negation rather than continuing the subtraction `(block-like) - operand`.
/// This is enforced in [`block_item_parser`] via [`block_like_head_parser`],
/// which parses a leading, statement-complete block-like expression on its own
/// (before the general Pratt expression that would otherwise absorb the `-`)
/// (RUE-210). Note this excludes `break`/`continue`/`return`, which are
/// diverging but not brace-terminated.
fn is_block_like(e: &Expr) -> bool {
    matches!(
        e,
        Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::Loop(_)
            | Expr::For(_)
            | Expr::Block(_)
    )
}

/// Returns true if the expression diverges (has the Never type).
/// These expressions can be promoted to the final expression of a block
/// since Never coerces to any type.
///
/// `loop` is included even though a loop containing a `break` has type `()`
/// rather than `!`: promotion is still transparent, because the block's value
/// becomes exactly the loop expression's value either way.
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
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
    block_like: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, BlockItem, ParserExtras<'src>> + Clone
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

    // Block-like head: a leading `if`/`match`/`while`/`loop`/`{ }` in statement
    // position that stands as a *complete* statement — one followed by a
    // boundary token (`-`, `;`, `}`, or EOF) rather than continued by an
    // operator/suffix — is parsed on its own, NOT through the Pratt `expr`
    // below. For a `-` follower this is a correctness rule: `expr` would read
    // the `-` as binary subtraction spanning two statements, so stopping right
    // after the block makes the `-` start a new statement (unary negation)
    // instead (RUE-210). For `;`/`}`/EOF followers it is a performance rule:
    // every nested block is `}`-terminated, so preferring this parser lets each
    // level be descended exactly once instead of being re-parsed by `expr`,
    // turning O(2^depth) block nesting into linear time (RUE-276). `block_like`
    // declines any other follower (a continuing operator/suffix) and `expr`
    // then handles the item unchanged. It is threaded in (rather than rebuilt
    // here) because it is mutually recursive with this parser through the block
    // bodies it contains, and it declines `break`/`return`/`continue`, which
    // fall through to `expr`.
    let block_like_head = block_like;

    // Expression-based item: parse expression ONCE, then decide based on what follows.
    // This avoids O(2^n) complexity from repeatedly re-parsing expressions with backtracking.
    //
    // The head is `block_like_head` (a statement-complete block-like expression)
    // or, failing that, the general Pratt `expr`. After parsing the head, we
    // check the next token:
    // - `;` → expression statement (value discarded)
    // - `}` → final expression (block's return value)
    // - other → mid-block control flow (only valid for if/while/match/loop/break/continue/return)
    let expr_item = choice((block_like_head, expr))
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
            span: Span::point_in_file(block_span.file_id, block_span.end),
        })
    });

    (statements, expr)
}

/// Parser for `{ statements... [expr] }` blocks, producing a [`BlockExpr`].
///
/// The final expression is optional: a block without one evaluates to unit
/// (see [`process_block_items`], which also promotes a trailing diverging
/// statement like `break`/`continue`/`return` to the final expression).
fn block_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
    block_like: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, BlockExpr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    block_item_parser(expr, block_like)
        .repeated()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace))
        .map_with(|items, e| {
            let span = span_from_extra(e);
            let (statements, final_expr) = process_block_items(items, span);
            BlockExpr {
                statements,
                expr: Box::new(final_expr),
                span,
            }
        })
}

/// Standalone parser for a *block-like* expression (`if`/`match`/`while`/
/// `loop`/`{ }`) that stands as a complete statement in statement position —
/// i.e. one that is NOT continued into a larger expression by a following
/// operator or postfix suffix. It succeeds only when the block-like construct is
/// immediately followed by a *statement-boundary* token: `-`, `;`, `}`, or
/// end-of-input (all non-consuming lookahead).
///
/// Two jobs, one parser:
///
///  * **The `-` boundary (RUE-210).** `-` is the only operator that is both a
///    prefix (unary) and an infix (binary) operator, so it is the only token
///    that can *silently* flip meaning across a statement boundary. Parsing the
///    block-like here, before the general Pratt expression, makes `{ a } - 1`
///    two statements (`{ a }` then unary `-1`) rather than the subtraction
///    `({ a }) - 1`. Unambiguously-binary operators (`+`, `*`, `==`, ...) and
///    postfix suffixes (`.`, `[`, ...) are *not* boundaries, so they fail the
///    lookahead and fall through to the general Pratt expression, which keeps a
///    leading block-like expression as their left operand (e.g. `{ a } + { b }`
///    stays one addition). The other prefix operators (`!`, `~`) are never
///    infix, so they already start a new statement without special handling.
///
///  * **Linear-time nesting (RUE-276).** Accepting `;`/`}`/end as boundaries
///    (not just `-`) is what keeps parsing near-linear in block-nesting depth.
///    A nested block `{ { { ... } } }` is always terminated by `}`, so this
///    parser *succeeds* at every level and the block-like is descended exactly
///    once. The narrow `-`-only predicate this replaced instead *failed* on
///    every `;`/`}`-terminated block, forcing the general Pratt `expr` to
///    re-descend the same nested input — two full descents per level, i.e.
///    O(2^depth) (depth 20 took >30s). The general `expr` is now reached only
///    for the rarer case of a block-like actually continued by an operator or
///    suffix, which is shallow in practice. Producing the same block-like `Expr`
///    the Pratt `expr` would, the AST is byte-identical either way.
///
/// It is `recursive` because the block bodies it contains hold block-items that
/// again prefer this parser; the recursion knot lets those bodies reference the
/// same parser lazily instead of triggering an infinite eager construction
/// cycle. The `try_map` declines `break`/`return`/`continue`, so those fall
/// through to the general Pratt expression and keep their optional operand.
fn block_like_head_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    recursive(|block_like| {
        choice((
            control_flow_parser(expr.clone(), block_like.clone()),
            block_parser(expr.clone(), block_like.clone()).map(Expr::Block),
        ))
        .try_map(|e, span| {
            if is_block_like(&e) {
                Ok(e)
            } else {
                Err(Rich::custom(span, "not a block-like expression"))
            }
        })
        // Engage only when the block-like is a *complete* statement — followed
        // by a boundary token (`-`, `;`, `}`, or EOF), non-consuming. Any other
        // follower (a binary operator or postfix suffix) means the block-like is
        // continued into a larger expression, so decline and let the Pratt
        // `expr` path handle it. `-` must be a boundary (RUE-210); the `;`/`}`/
        // EOF boundaries keep deeply-nested blocks linear (RUE-276).
        .then_ignore(
            choice((
                just(TokenKind::Minus).ignored(),
                just(TokenKind::Semi).ignored(),
                just(TokenKind::RBrace).ignored(),
                end(),
            ))
            .rewind(),
        )
        .boxed()
    })
}

/// Parser for function definitions: [@directive]* [pub] [unchecked] fn name(params) -> Type { body }
fn function_parser<'src, I>() -> impl Parser<'src, I, Function, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    directives_parser()
        .then(visibility)
        .then(just(TokenKind::Unchecked).or_not())
        .then(just(TokenKind::Fn).ignore_then(ident_parser()))
        .then(params_parser().delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .then(just(TokenKind::Arrow).ignore_then(type_parser()).or_not())
        .then(block_parser(expr.clone(), block_like_head_parser(expr)))
        .map_with(
            |((((((directives, visibility), is_unchecked), name), params), return_type), body),
             e| Function {
                directives,
                visibility,
                is_unchecked: is_unchecked.is_some(),
                name,
                params,
                return_type,
                body: Expr::Block(body),
                span: span_from_extra(e),
            },
        )
}

/// Parser for struct definitions with inline methods:
/// [@directive]* [pub] [linear] struct Name { field: Type, ... fn method(self) { ... } }
///
/// Fields come first (comma-separated), then methods (no separators needed).
fn struct_parser<'src, I>() -> impl Parser<'src, I, StructDecl, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    // Parse the struct body: fields followed by methods
    // Fields are comma-separated, methods are not
    let struct_body = field_decls_parser()
        .then(method_parser().repeated().collect::<Vec<_>>())
        .map(|(fields, methods)| (fields, methods));

    directives_parser()
        .then(visibility)
        .then(just(TokenKind::Linear).or_not())
        .then(just(TokenKind::Struct).ignore_then(ident_parser()))
        .then(struct_body.delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(
            |((((directives, visibility), is_linear), name), (fields, methods)), e| StructDecl {
                directives,
                visibility,
                is_linear: is_linear.is_some(),
                name,
                fields,
                methods,
                span: span_from_extra(e),
            },
        )
}

/// Parser for enum variant: `Name` (discriminant-only) or `Name(T1, T2, ...)`
/// (tuple variant carrying payload data, RUE-221).
fn enum_variant_parser<'src, I>() -> impl Parser<'src, I, EnumVariant, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Optional tuple payload: `(T1, T2, ...)`. At least one type required
    // inside the parens; `V()` is not a valid tuple variant.
    let payload = type_parser()
        .separated_by(just(TokenKind::Comma))
        .at_least(1)
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(TokenKind::LParen), just(TokenKind::RParen))
        .or_not()
        .map(|opt| opt.unwrap_or_default());

    ident_parser()
        .then(payload)
        .map_with(|(name, payload), e| EnumVariant {
            name,
            payload,
            span: span_from_extra(e),
        })
}

/// Parser for comma-separated enum variants
fn enum_variants_parser<'src, I>()
-> impl Parser<'src, I, Vec<EnumVariant>, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    enum_variant_parser()
        .separated_by(just(TokenKind::Comma))
        .allow_trailing()
        .collect()
}

/// Parser for enum definitions: [pub] enum Name { Variant1, Variant2, ... }
fn enum_parser<'src, I>() -> impl Parser<'src, I, EnumDecl, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    visibility
        .then(just(TokenKind::Enum).ignore_then(ident_parser()))
        .then(enum_variants_parser().delimited_by(just(TokenKind::LBrace), just(TokenKind::RBrace)))
        .map_with(|((visibility, name), variants), e| EnumDecl {
            visibility,
            name,
            variants,
            span: span_from_extra(e),
        })
}

/// Parser for method definitions: [@directive]* fn name(self, params) -> Type { body }
/// Methods differ from functions in that they can have `self` as the first parameter.
fn method_parser<'src, I>() -> impl Parser<'src, I, Method, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();
    method_parser_with_expr(expr)
}

/// Parser for method definitions inside anonymous structs.
/// Takes an expression parser as a parameter to avoid creating a new one.
fn anon_struct_method_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Method, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    method_parser_with_expr(expr)
}

/// Parser for a destructor declared *inside* an anonymous struct body:
/// `drop fn(self) { body }` (RUE-312).
///
/// A named struct's destructor is a top-level `drop fn Name(self)`; an
/// anonymous struct (the one a comptime `-> type` function returns) has no
/// name to key on, so its destructor lives in the struct body. To reuse the
/// existing anon-struct method monomorphization + drop-glue machinery, it is
/// modeled as an ordinary [`Method`] under the reserved name `__drop`, with a
/// by-value `self` receiver — matching how the drop glue invokes a destructor
/// (by value). Sema recognizes the `__drop` name and registers it as the
/// monomorphized struct's `{name}.__drop` destructor.
fn anon_struct_drop_fn_parser<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Method, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Destructors always take `self` by value (see the drop-glue call site,
    // which passes the receiver in `Normal` mode). Only bare `self` is
    // accepted, mirroring the top-level `drop fn Name(self)` grammar.
    let self_param = just(TokenKind::SelfValue).map_with(|_, e| SelfParam {
        mode: ParamMode::Normal,
        span: span_from_extra(e),
    });

    just(TokenKind::Drop)
        .ignore_then(just(TokenKind::Fn))
        .ignore_then(self_param.delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .then(block_parser(expr.clone(), block_like_head_parser(expr)))
        .map_with(|(self_param, body), e| {
            let span = span_from_extra(e);
            let syms = e.state().0.syms;
            Method {
                directives: smallvec::SmallVec::new(),
                name: Ident {
                    name: syms.drop_marker,
                    span,
                },
                receiver: Some(self_param),
                params: Vec::new(),
                return_type: None,
                body: Expr::Block(body),
                span,
            }
        })
}

/// Shared implementation for method parsing.
/// Takes an expression parser to allow reuse from different contexts.
fn method_parser_with_expr<'src, I>(
    expr: impl Parser<'src, I, Expr, ParserExtras<'src>> + Clone + 'src,
) -> impl Parser<'src, I, Method, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Parse optional self parameter, with an optional receiver mode keyword
    // (`borrow self` / `inout self`), mirroring the parameter modes (RUE-15).
    // Bare `self` stays by-value (`Normal`).
    let receiver_mode = choice((
        just(TokenKind::Inout).to(ParamMode::Inout),
        just(TokenKind::Borrow).to(ParamMode::Borrow),
    ))
    .or_not()
    .map(|opt| opt.unwrap_or(ParamMode::Normal));

    let self_param = receiver_mode
        .then(just(TokenKind::SelfValue))
        .map_with(|(mode, _), e| SelfParam {
            mode,
            span: span_from_extra(e),
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
        .then(block_parser(expr.clone(), block_like_head_parser(expr)))
        .map_with(
            |((((directives, name), (receiver, params)), return_type), body), e| Method {
                directives,
                name,
                receiver,
                params,
                return_type,
                body: Expr::Block(body),
                span: span_from_extra(e),
            },
        )
}

/// Parser for drop fn declarations: drop fn TypeName(self) { body }
fn drop_fn_parser<'src, I>() -> impl Parser<'src, I, DropFn, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse self parameter (destructors always take self by value)
    let self_param = just(TokenKind::SelfValue).map_with(|_, e| SelfParam {
        mode: ParamMode::Normal,
        span: span_from_extra(e),
    });

    just(TokenKind::Drop)
        .ignore_then(just(TokenKind::Fn))
        .ignore_then(ident_parser())
        .then(self_param.delimited_by(just(TokenKind::LParen), just(TokenKind::RParen)))
        .then(block_parser(expr.clone(), block_like_head_parser(expr)))
        .map_with(|((type_name, self_param), body), e| DropFn {
            type_name,
            self_param,
            body: Expr::Block(body),
            span: span_from_extra(e),
        })
}

/// Parser for const declarations: [pub] const name [: Type] = expr;
///
/// Used for module re-exports:
/// ```rue
/// pub const strings = @import("utils/strings.rue");
/// pub const helper = @import("utils/internal.rue").helper;
/// ```
fn const_parser<'src, I>() -> impl Parser<'src, I, ConstDecl, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    let expr = expr_parser();

    // Parse optional visibility (pub keyword)
    let visibility = just(TokenKind::Pub).or_not().map(|opt| {
        if opt.is_some() {
            Visibility::Public
        } else {
            Visibility::Private
        }
    });

    // Optional type annotation
    let type_annotation = just(TokenKind::Colon).ignore_then(type_parser()).or_not();

    directives_parser()
        .then(visibility)
        .then(just(TokenKind::Const).ignore_then(ident_parser()))
        .then(type_annotation)
        .then(just(TokenKind::Eq).ignore_then(expr))
        .then_ignore(just(TokenKind::Semi))
        .map_with(
            |((((directives, visibility), name), ty), init), e| ConstDecl {
                directives,
                visibility,
                name,
                ty,
                init: Box::new(init),
                span: span_from_extra(e),
            },
        )
}

/// Parser for top-level items (functions, structs, enums, drop fns, and consts)
fn item_parser<'src, I>() -> impl Parser<'src, I, Item, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        function_parser().map(Item::Function),
        struct_parser().map(Item::Struct),
        enum_parser().map(Item::Enum),
        drop_fn_parser().map(Item::DropFn),
        const_parser().map(Item::Const),
    ))
}

/// Parser that matches tokens that can start an item (for recovery).
/// This is a "lookahead" - it peeks but doesn't consume.
fn item_start<'src, I>() -> impl Parser<'src, I, (), ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    choice((
        just(TokenKind::Fn).ignored(),
        just(TokenKind::Struct).ignored(),
        just(TokenKind::Enum).ignored(),
        just(TokenKind::Drop).ignored(),
        just(TokenKind::Const).ignored(),
        just(TokenKind::Pub).ignored(),
        just(TokenKind::Linear).ignored(),
        just(TokenKind::Unchecked).ignored(),
        just(TokenKind::At).ignored(), // For @directives
    ))
    .rewind() // Peek without consuming
}

/// Recovery parser that skips tokens until finding an item start.
/// Consumes at least one token to guarantee progress, then skips until
/// we find a token that could start an item.
fn error_recovery<'src, I>() -> impl Parser<'src, I, Item, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    // Skip at least one token to make progress, capturing the span
    any()
        .map_with(|_, extra| extra.span())
        // Then skip any more tokens that don't start an item
        .then(
            any()
                .and_is(item_start().not())
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map_with(|(start_span, _): (SimpleSpan, Vec<TokenKind>), extra| {
            // Convert SimpleSpan to Span, tagging it with this file's ID
            let file_id = extra.state().0.file_id;
            Item::Error(to_rue_span_with_file(start_span, file_id))
        })
}

/// Parser for top-level items with error recovery.
/// When an item fails to parse, we skip tokens until we find the start of
/// another item, emit an Error node, and continue parsing.
fn item_with_recovery<'src, I>() -> impl Parser<'src, I, Item, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    item_parser().recover_with(via_parser(error_recovery()))
}

/// Main parser that produces an AST
fn ast_parser<'src, I>() -> impl Parser<'src, I, Ast, ParserExtras<'src>> + Clone
where
    I: ValueInput<'src, Token = TokenKind, Span = SimpleSpan>,
{
    item_with_recovery()
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
///
/// The `file_id` is threaded in from the parser so that error spans point at
/// the file being parsed; without it, spans would default to `FileId::DEFAULT`
/// and multi-file compilations would attribute errors to the wrong file.
fn convert_error(err: Rich<'_, TokenKind>, file_id: FileId) -> CompileError {
    let span = to_rue_span_with_file(*err.span(), file_id);

    // Build the base error from the reason
    let mut error = match err.reason() {
        chumsky::error::RichReason::ExpectedFound { expected, found } => {
            let expected_str: Cow<'static, str> = if expected.is_empty() {
                Cow::Borrowed("something")
            } else {
                // Format every expected pattern, then dedupe: chumsky can list
                // the *same* pattern once per failed alternative that wanted it
                // (e.g. `identifier` from several branches). Preserve order.
                let mut names: Vec<String> = Vec::new();
                for pat in expected.iter() {
                    let s = format_pattern(pat);
                    if !names.contains(&s) {
                        names.push(s);
                    }
                }
                // Drop the useless "something else" catch-all whenever a
                // concrete alternative is present — it carries no information
                // next to a real token or a named context. Only when it is the
                // *sole* expectation do we keep it as a last resort. (RUE-19)
                if names.iter().any(|n| n != "something else") {
                    names.retain(|n| n != "something else");
                }
                // When a parser labels a whole syntactic context, prefer that
                // context over low-level first tokens from alternatives inside
                // it. For example, an empty expression after `=` should say
                // `expected expression`, not enumerate every unary-prefix
                // token plus `expression`. (RUE-19)
                if names.iter().any(|n| n == "expression") {
                    names.retain(|n| n == "expression");
                }
                // Cap the enumeration so a position that accepts many operators
                // does not produce a wall of text.
                const MAX_EXPECTED: usize = 4;
                if names.len() > MAX_EXPECTED {
                    names.truncate(MAX_EXPECTED);
                    names.push("…".to_string());
                }
                Cow::Owned(names.join(" or "))
            };

            let found_str: Cow<'static, str> = match found.as_ref() {
                Some(t) => Cow::Owned(t.name().to_string()),
                None => Cow::Borrowed("end of file"),
            };

            // `self` is only meaningful as a method's first parameter. When it
            // turns up anywhere else the raw "expected ..., found 'self'" is
            // opaque, so point the user at the rule. (RUE-19)
            let found_is_self =
                matches!(found.as_ref(), Some(t) if t.name() == TokenKind::SelfValue.name());
            // `impl` is Rust muscle memory: Rue has no `impl` blocks. It is a
            // reserved keyword (spec 2.4:2, RUE-331), so it reaches the parser as
            // a dedicated `Impl` token and dies at item position with an opaque
            // "expected item, found 'impl'". Point the user at where methods
            // actually go. (RUE-19 item 4)
            let found_is_impl = matches!(found.as_ref(), Some(t) if matches!(**t, TokenKind::Impl));
            // `::` was removed as a path/member-access operator (RUE-488): `.`
            // is the sole spelling for enum variants (`Enum.Variant`),
            // associated-function calls (`Type.function()`), and module-member
            // access. The token still lexes, so a stray `::` reaches the parser
            // and dies here; point the user at `.` rather than leaving an
            // opaque "unexpected '::'".
            let found_is_coloncolon =
                matches!(found.as_ref(), Some(t) if matches!(**t, TokenKind::ColonColon));
            let mut err = CompileError::new(
                ErrorKind::UnexpectedToken {
                    expected: expected_str,
                    found: found_str,
                },
                span,
            );
            if found_is_self {
                err = err.with_help("methods take `self` as the first parameter");
            } else if found_is_impl {
                err = err.with_help(
                    "Rue has no `impl` blocks; define methods inside the struct body, \
                     e.g. `struct S { x: i32, fn m(self) -> i32 { self.x } }`",
                );
            } else if found_is_coloncolon {
                err = err.with_help("`::` is not a Rue operator; use `.` for member access, e.g. `Enum.Variant` or `Type.function()`");
            }
            err
        }
        chumsky::error::RichReason::Custom(msg) => {
            // Preserve the custom error message directly
            CompileError::new(ErrorKind::ParseError(msg.clone()), span)
        }
    };

    // Add labelled contexts as secondary labels
    for (pattern, ctx_span) in err.contexts() {
        let label_msg = format!("while parsing {}", format_pattern(pattern));
        let label_span = to_rue_span_with_file(*ctx_span, file_id);
        error = error.with_label(label_msg, label_span);
    }

    error
}

/// Drop byte-for-byte duplicate parse errors.
///
/// chumsky can surface the *same* diagnostic more than once when several parse
/// alternatives fail at the same token — e.g. `self` in non-receiver position
/// fails both the self-parameter and the regular-parameter branch, producing
/// the identical E0100 twice for one span. Two errors are considered duplicates
/// only when their kind (code + message payload) and span match exactly, so
/// genuinely distinct problems at the same location are preserved. (RUE-19)
fn dedupe_parse_errors(errors: &mut Vec<CompileError>) {
    // Error lists are tiny, so a linear "seen" scan is cheaper than hashing.
    let mut seen: Vec<(Option<Span>, ErrorKind)> = Vec::new();
    errors.retain(|e| {
        let key = (e.span(), e.kind.clone());
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}

/// Scan the token stream for excessive syntactic nesting, returning an error
/// if the input would nest deeper than [`MAX_NESTING_DEPTH`].
///
/// Deeply nested input can overflow the stack in three distinct places, and
/// this single pre-pass guards all of them (RUE-42):
///
/// 1. **The parser itself.** chumsky's recursive combinators descend one native
///    stack frame per level of bracketed expression (`(`/`[`/`{`), nested type
///    (`[T; N]`, `ptr const T`), and `else if` chain.
/// 2. **AstGen.** Lowering walks the AST recursively, so a deep tree — even one
///    built iteratively by the Pratt parser, like `1 + 1 + ... + 1` or
///    `a.b.c...` — overflows during lowering.
/// 3. **`Drop`.** The AST's boxed children drop recursively, so merely *holding*
///    a deep tree and letting it fall out of scope overflows.
///
/// Because it runs *before* the tokens reach chumsky, rejecting here means no
/// pass (parse, lower, or drop) ever sees a tree deep enough to crash.
///
/// The scan maintains one operator/wrap counter per open bracket level. The
/// tracked value `(levels.len() - 1) + sum(levels)` is a sound upper bound on
/// the depth of the AST that would be produced: every construct that adds a
/// level of AST nesting — an opening bracket, a prefix/infix operator, a
/// postfix `.`/`[`/`?`, an `else`, or a `ptr` — bumps it by at least one. Counters
/// reset at expression separators (`;`, `,`, `=>`, `=`, `:`, `->`) so that a
/// long *sequence* of shallow expressions (many statements, many call
/// arguments, many match arms) is not mistaken for deep nesting. The bound can
/// still over-count some shapes, which only makes the guard slightly more
/// conservative — never less sound. A postfix index `[` and an array
/// type/literal `[` are distinguished by the preceding token so that nested
/// array types (`[[..T..]]`) count once per level, honoring the 256-level
/// minimum instead of rejecting at 128 (RUE-533).
fn check_nesting_depth(
    tokens: &[(TokenKind, SimpleSpan)],
    file_id: FileId,
) -> Option<CompileError> {
    // Per-bracket-level operator/wrap counters; `levels[0]` is the top level.
    let mut levels: Vec<usize> = vec![0];
    // Running sum of `levels`, maintained incrementally so the depth check is
    // O(1) per token rather than O(n).
    let mut total_ops: usize = 0;

    // Current upper bound on AST depth given the open brackets and pending
    // operators/wraps.
    macro_rules! depth {
        () => {
            (levels.len() - 1) + total_ops
        };
    }

    // The previous significant token, so a `[` can be classified as either a
    // postfix index (`expr[i]`) or an array wrap/open (`[T; N]`, `[a, b]`).
    // Only a `[` that follows a value-producing token is a postfix index
    // (RUE-533).
    let mut prev: Option<&TokenKind> = None;
    macro_rules! reject_if_too_deep {
        ($span:expr) => {
            if depth!() > MAX_NESTING_DEPTH {
                return Some(CompileError::new(
                    ErrorKind::NestingLimitExceeded {
                        limit: MAX_NESTING_DEPTH,
                    },
                    to_rue_span_with_file(*$span, file_id),
                ));
            }
        };
    }

    for (kind, span) in tokens {
        match kind {
            // Grouping / call / block / struct-literal open: one new nesting
            // level.
            TokenKind::LParen | TokenKind::LBrace => {
                levels.push(0);
                reject_if_too_deep!(span);
            }
            // `[` opens a new nested level for the expression/type inside it.
            // It ALSO wraps the current level in one AST node ONLY when it is a
            // postfix index (`expr[i]` → an Index node); an array type or
            // literal (`[T; N]`, `[a, b]`) in value/type position adds just the
            // one nested level. Counting the wrap unconditionally double-counts
            // array nesting and halved the effective limit for `[[..T..]]`,
            // rejecting at 128 levels despite the 256 minimum (RUE-533). A `[`
            // is a postfix index exactly when it follows a value-producing
            // token.
            TokenKind::LBracket => {
                let is_postfix_index = matches!(
                    prev,
                    Some(
                        TokenKind::Ident(_)
                            | TokenKind::Int(_)
                            | TokenKind::String(_)
                            | TokenKind::True
                            | TokenKind::False
                            | TokenKind::SelfValue
                            | TokenKind::RParen
                            | TokenKind::RBracket
                            | TokenKind::RBrace
                    )
                );
                if is_postfix_index {
                    *levels.last_mut().unwrap() += 1;
                    total_ops += 1;
                }
                levels.push(0);
                reject_if_too_deep!(span);
            }
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                if levels.len() > 1 {
                    total_ops -= levels.pop().unwrap();
                }
            }
            // Expression / statement / clause separators: the sub-expression at
            // the current level is complete, so its operators no longer
            // contribute to depth.
            TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::FatArrow
            | TokenKind::Eq
            | TokenKind::Colon
            | TokenKind::Arrow => {
                total_ops -= *levels.last().unwrap();
                *levels.last_mut().unwrap() = 0;
            }
            // Prefix / infix operators, postfix field access, `else if` chains,
            // and pointer types each wrap the surrounding expression/type in one
            // more AST node.
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::LtEq
            | TokenKind::GtEq
            | TokenKind::AmpAmp
            | TokenKind::PipePipe
            | TokenKind::Amp
            | TokenKind::Pipe
            | TokenKind::Caret
            | TokenKind::Tilde
            | TokenKind::LtLt
            | TokenKind::GtGt
            | TokenKind::Bang
            | TokenKind::Dot
            | TokenKind::Question
            | TokenKind::Else
            | TokenKind::Ptr => {
                *levels.last_mut().unwrap() += 1;
                total_ops += 1;
                reject_if_too_deep!(span);
            }
            _ => {}
        }
        prev = Some(kind);
    }
    None
}

/// Chumsky-based parser that converts tokens into an AST.
pub struct ChumskyParser {
    tokens: Vec<(TokenKind, SimpleSpan)>,
    source_len: usize,
    interner: ThreadedRodeo,
    /// File ID for spans in this file.
    file_id: FileId,
}

impl ChumskyParser {
    /// Create a new parser from tokens and an interner produced by the lexer.
    pub fn new(tokens: Vec<rue_lexer::Token>, interner: ThreadedRodeo) -> Self {
        let source_len = tokens.last().map(|t| t.span.end as usize).unwrap_or(0);
        // Extract file_id from the first token (all tokens in a file have the same file_id)
        let file_id = tokens
            .first()
            .map(|t| t.span.file_id)
            .unwrap_or(FileId::DEFAULT);

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
            file_id,
        }
    }

    /// Parse the tokens into an AST, returning the AST and the interner.
    ///
    /// Returns all parse errors if parsing fails, not just the first one.
    pub fn parse(self) -> MultiErrorResult<(Ast, ThreadedRodeo)> {
        self.parse_preserving_interner()
            .map_err(|(errors, _interner)| errors)
    }

    /// Parse the tokens into an AST, preserving the interner even on error.
    ///
    /// Multi-file compilation uses this to continue parsing later files after
    /// an earlier file fails, while still sharing one interner across all files.
    pub fn parse_preserving_interner(
        mut self,
    ) -> Result<(Ast, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        // Guard against pathologically nested input BEFORE handing tokens to
        // chumsky. The recursive-descent parser descends one level per opening
        // bracket, so combined bracket-nesting depth is an upper bound on
        // parser recursion depth; rejecting deep input here means chumsky (and
        // every later pass, plus the recursive `Drop` of the AST) never sees a
        // tree deep enough to overflow the stack (RUE-42).
        if let Some(err) = check_nesting_depth(&self.tokens, self.file_id) {
            return Err((CompileErrors::from(vec![err]), self.interner));
        }

        // Pre-intern primitive type symbols and create parser state with file ID
        let syms = PrimitiveTypeSpurs::new(&mut self.interner);
        let parser_state = ParserState::new(syms, self.file_id);

        // Run chumsky on a dedicated thread with a large stack. Even at the
        // bounded `MAX_NESTING_DEPTH`, chumsky's combinator frames are heavy
        // (tens of KB per nesting level), so a 256-deep parse would blow the
        // default thread stack. The nesting pre-scan above caps the depth; the
        // roomy stack here ensures the capped depth parses cleanly.
        let tokens = std::mem::take(&mut self.tokens);
        let source_len = self.source_len;
        let file_id = self.file_id;
        let result = std::thread::Builder::new()
            .name("rue-parse".to_string())
            .stack_size(256 * 1024 * 1024)
            .spawn(move || {
                let mut state = SimpleState(parser_state);

                // Create a stream from the token iterator
                let token_iter = tokens.iter().cloned();
                let stream = Stream::from_iter(token_iter);

                // Map the stream to split (Token, Span) tuples
                let eoi: SimpleSpan = (source_len..source_len).into();
                let mapped = stream.map(eoi, |(tok, span)| (tok, span));

                ast_parser()
                    .parse_with_state(mapped, &mut state)
                    .into_result()
                    .map_err(|errs| {
                        let mut errors: Vec<CompileError> = errs
                            .into_iter()
                            .map(|err| convert_error(err, file_id))
                            .collect();
                        dedupe_parse_errors(&mut errors);
                        CompileErrors::from(errors)
                    })
            })
            .expect("failed to spawn parser thread")
            .join()
            .expect("parser thread panicked");

        let ast = match result {
            Ok(ast) => ast,
            Err(errors) => return Err((errors, self.interner)),
        };

        // Post-parse validation that needs the interner (e.g. resolving
        // directive names so the diagnostic can say which directive is
        // unknown). See the `validate` module.
        let validation_errors = crate::validate::check_directives(&ast, &self.interner);
        if !validation_errors.is_empty() {
            return Err((CompileErrors::from(validation_errors), self.interner));
        }

        Ok((ast, self.interner))
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
            Item::DropFn(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Const(_) => panic!("parse_expr helper should only be used with functions"),
            Item::Error(_) => panic!("parse_expr helper should only be used with functions"),
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
            Item::DropFn(_) => panic!("expected Function"),
            Item::Const(_) => panic!("expected Function"),
            Item::Error(_) => panic!("expected Function"),
        }
    }

    /// Regression: deeply nested blocks must parse in (near-)linear time.
    ///
    /// The RUE-210 `block_like_head_parser` used to require a `-` to follow a
    /// leading block-like construct; for every `;`/`}`-terminated block it
    /// instead FAILED and the general Pratt `expr` re-descended the same nested
    /// input, giving O(2^depth) behavior (RUE-276): depth 16 took ~1.6s, depth
    /// 20 timed out (>30s). Accepting `;`/`}`/EOF as statement boundaries makes
    /// each level descend exactly once. Depth 60 here parses effectively
    /// instantly; if the exponential blowup ever returns this test hangs, which
    /// the harness surfaces as a timeout.
    #[test]
    fn test_deeply_nested_blocks_parse_linearly() {
        const DEPTH: usize = 60;
        let source = format!(
            "fn main() -> i32 {{ {} 0 {} }}",
            "{ ".repeat(DEPTH),
            "}".repeat(DEPTH),
        );
        let result = parse(&source).expect("deeply nested blocks should parse");
        // Walk the nested-block spine and confirm the tree is actually DEPTH+1
        // blocks deep (function body block plus DEPTH inner blocks), so the test
        // exercises real nesting rather than a collapsed/error tree.
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let mut expr = &f.body;
        let mut levels = 0;
        while let Expr::Block(block) = expr {
            levels += 1;
            expr = &block.expr;
        }
        assert_eq!(levels, DEPTH + 1, "expected fully nested block spine");
        assert!(matches!(expr, Expr::Int(_)), "innermost value is `0`");
    }

    /// Regression: nested array-list literals must parse in linear time.
    ///
    /// RUE-374: array literals used to be parsed as `[expr ; count]` OR
    /// `[expr, ...]`. For a nested list like `[[[1]]]`, every level first parsed
    /// the full inner array as a repeat value, failed to find `;`, then
    /// backtracked and parsed the same inner array again as a list element. That
    /// doubled the work per level. The parser now parses the first expression
    /// once and dispatches on the following separator.
    #[test]
    fn test_deeply_nested_array_lists_parse_linearly() {
        const DEPTH: usize = 60;
        let source = format!("{}1{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
        let result = parse_expr(&source).expect("deeply nested array list should parse");

        let mut expr = &result.expr;
        let mut levels = 0;
        while let Expr::ArrayLit(array) = expr {
            levels += 1;
            assert_eq!(array.elements.len(), 1, "expected single-element list");
            assert!(
                array.repeat.is_none(),
                "nested list must not parse as repeat"
            );
            expr = &array.elements[0];
        }
        assert_eq!(levels, DEPTH, "expected fully nested array-list spine");
        assert!(matches!(expr, Expr::Int(_)), "innermost value is `1`");
    }

    #[test]
    fn test_array_literal_forms() {
        let empty = parse_expr("[]").unwrap();
        let Expr::ArrayLit(array) = empty.expr else {
            panic!("expected empty array literal");
        };
        assert!(array.elements.is_empty());
        assert!(array.repeat.is_none());

        let list = parse_expr("[1, 2]").unwrap();
        let Expr::ArrayLit(array) = list.expr else {
            panic!("expected array-list literal");
        };
        assert_eq!(array.elements.len(), 2);
        assert!(array.repeat.is_none());

        let repeat = parse_expr("[1; 4]").unwrap();
        let Expr::ArrayLit(array) = repeat.expr else {
            panic!("expected array-repeat literal");
        };
        assert_eq!(array.elements.len(), 1);
        assert!(matches!(array.repeat, Some(ArrayLength::Literal(4))));
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
            Item::DropFn(_) => panic!("expected Function"),
            Item::Const(_) => panic!("expected Function"),
            Item::Error(_) => panic!("expected Function"),
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
    fn test_empty_if_body_with_ident_condition() {
        let result = parse("fn main() -> i32 { if done {} 0 }").unwrap();
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(block) = &f.body else {
            panic!("expected block body");
        };
        let Statement::Expr(Expr::If(if_expr)) = &block.statements[0] else {
            panic!("expected first statement to be an If expression");
        };
        let Expr::Ident(cond) = if_expr.cond.as_ref() else {
            panic!("expected identifier condition, got {:?}", if_expr.cond);
        };
        assert_eq!(result.get(cond.name), "done");
        assert!(if_expr.then_block.statements.is_empty());
        assert!(matches!(if_expr.then_block.expr.as_ref(), Expr::Unit(_)));
    }

    #[test]
    fn test_empty_if_body_with_field_condition() {
        let result = parse("fn main() -> i32 { if state.done {} 0 }").unwrap();
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(block) = &f.body else {
            panic!("expected block body");
        };
        let Statement::Expr(Expr::If(if_expr)) = &block.statements[0] else {
            panic!("expected first statement to be an If expression");
        };
        let Expr::Field(cond) = if_expr.cond.as_ref() else {
            panic!("expected field condition, got {:?}", if_expr.cond);
        };
        assert_eq!(result.get(cond.field.name), "done");
        let Expr::Ident(base) = cond.base.as_ref() else {
            panic!("expected identifier field base, got {:?}", cond.base);
        };
        assert_eq!(result.get(base.name), "state");
        assert!(if_expr.then_block.statements.is_empty());
        assert!(matches!(if_expr.then_block.expr.as_ref(), Expr::Unit(_)));
    }

    #[test]
    fn test_empty_while_body_with_ident_condition() {
        let result = parse("fn main() -> i32 { while done {} 0 }").unwrap();
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(block) = &f.body else {
            panic!("expected block body");
        };
        let Statement::Expr(Expr::While(while_expr)) = &block.statements[0] else {
            panic!("expected first statement to be a While expression");
        };
        let Expr::Ident(cond) = while_expr.cond.as_ref() else {
            panic!("expected identifier condition, got {:?}", while_expr.cond);
        };
        assert_eq!(result.get(cond.name), "done");
        assert!(while_expr.body.statements.is_empty());
        assert!(matches!(while_expr.body.expr.as_ref(), Expr::Unit(_)));
    }

    #[test]
    fn test_empty_for_body_with_ident_iterable() {
        let result = parse("fn main() -> i32 { for item in items {} 0 }").unwrap();
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(block) = &f.body else {
            panic!("expected block body");
        };
        let Statement::Expr(Expr::For(for_expr)) = &block.statements[0] else {
            panic!("expected first statement to be a For expression");
        };
        let Expr::Ident(iterable) = for_expr.iterable.as_ref() else {
            panic!("expected identifier iterable, got {:?}", for_expr.iterable);
        };
        assert_eq!(result.get(iterable.name), "items");
        assert!(for_expr.body.statements.is_empty());
        assert!(matches!(for_expr.body.expr.as_ref(), Expr::Unit(_)));
    }

    #[test]
    fn test_bare_struct_literal_if_condition_is_targeted_error() {
        let lines = error_lines("fn main() -> i32 { if Point { x: 1 } {} 0 }");
        assert!(
            lines.iter().any(|line| line.contains(
                "struct literals are not allowed as a bare if condition"
            )),
            "{lines:?}"
        );
    }

    #[test]
    fn test_parenthesized_struct_literal_if_condition_still_parses() {
        let result = parse("fn main() -> i32 { if (Point { x: 1 }) {} 0 }");
        assert!(
            result.is_ok(),
            "parenthesized struct literal conditions remain syntactically valid"
        );
    }

    #[test]
    fn test_for_over_array() {
        // `for x in <iterable> { body }` parses as a For expression (RUE-220).
        let result = parse("fn main() -> i32 { for x in a { s = s + x; } 0 }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(block) = &f.body else {
            panic!("expected block body");
        };
        let Statement::Expr(Expr::For(for_expr)) = &block.statements[0] else {
            panic!("expected first statement to be a For expression");
        };
        assert!(matches!(for_expr.binder, LetPattern::Ident(_)));
    }

    #[test]
    fn test_for_wildcard_and_chars() {
        // Wildcard binder and a `.chars()` method-call iterable both parse.
        let result = parse("fn main() -> i32 { for _ in s.chars() { n = n + 1; } 0 }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
        let Item::Function(f) = &result.ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(block) = &f.body else {
            panic!("expected block body");
        };
        let Statement::Expr(Expr::For(for_expr)) = &block.statements[0] else {
            panic!("expected first statement to be a For expression");
        };
        assert!(matches!(for_expr.binder, LetPattern::Wildcard(_)));
        assert!(matches!(&*for_expr.iterable, Expr::MethodCall(_)));
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

    // ==================== Struct Method Parsing Tests ====================

    #[test]
    fn test_struct_with_single_method() {
        let result = parse("struct Point { x: i32, fn get_x(self) -> i32 { self.x } }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                assert_eq!(result.get(struct_decl.name.name), "Point");
                assert_eq!(struct_decl.methods.len(), 1);
                let method = &struct_decl.methods[0];
                assert_eq!(result.get(method.name.name), "get_x");
                assert!(method.receiver.is_some()); // has self
                assert!(method.params.is_empty()); // no additional params
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_method_with_params() {
        let result =
            parse("struct Point { x: i32, fn add(self, n: i32) -> i32 { self.x + n } }").unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                assert_eq!(result.get(method.name.name), "add");
                assert!(method.receiver.is_some());
                assert_eq!(method.params.len(), 1);
                assert_eq!(result.get(method.params[0].name.name), "n");
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_associated_function() {
        // Associated function (no self)
        let result = parse(
            "struct Point { x: i32, y: i32, fn new(x: i32, y: i32) -> Point { Point { x: x, y: y } } }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                assert_eq!(result.get(method.name.name), "new");
                assert!(method.receiver.is_none()); // no self
                assert_eq!(method.params.len(), 2);
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_multiple_methods() {
        let result = parse(
            "struct Counter {
                 value: i32,
                 fn new() -> Counter { Counter { value: 0 } }
                 fn get(self) -> i32 { self.value }
                 fn increment(self) -> i32 { self.value + 1 }
             }",
        )
        .unwrap();
        assert_eq!(result.ast.items.len(), 1);
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                assert_eq!(struct_decl.methods.len(), 3);
                // First is associated function (no self)
                assert!(struct_decl.methods[0].receiver.is_none());
                assert_eq!(result.get(struct_decl.methods[0].name.name), "new");
                // Second is method (has self)
                assert!(struct_decl.methods[1].receiver.is_some());
                assert_eq!(result.get(struct_decl.methods[1].name.name), "get");
                // Third is method (has self)
                assert!(struct_decl.methods[2].receiver.is_some());
                assert_eq!(result.get(struct_decl.methods[2].name.name), "increment");
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_struct_method_with_directive() {
        // Must use a KNOWN directive: unknown ones are rejected post-parse
        // by `validate::check_directives` (RUE-133).
        let result =
            parse("struct Foo { @allow(unused_variable) fn bar(self) -> i32 { 42 } }").unwrap();
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                assert_eq!(method.directives.len(), 1);
                assert_eq!(result.get(method.directives[0].name.name), "allow");
            }
            _ => panic!("expected Struct"),
        }
    }

    #[test]
    fn test_unknown_directive_rejected() {
        // RUE-133: unknown @-directives in item position used to be silently
        // accepted and ignored (e.g. `@typo_allow` compiling as a no-op).
        let err = parse("@important\nfn main() -> i32 { 42 }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("unknown directive '@important'"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_unknown_directive_on_nested_let_rejected() {
        let err = parse(
            "fn main() -> i32 { if true { @alllow(unused_variable) let x = 1; x } else { 0 } }",
        )
        .unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("unknown directive '@alllow'"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_misplaced_block_directive_rejected() {
        // RUE-403: a directive without a following statement used to parse as
        // an intrinsic-like expression and surface later as a type mismatch.
        let err = parse("fn main() -> i32 { @allow(unused_variable) }").unwrap_err();
        let messages = err
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            messages.contains("directive must precede a statement"),
            "unexpected messages: {messages}"
        );
    }

    #[test]
    fn test_unknown_allow_warning_rejected() {
        let err = parse("fn main() -> i32 { @allow(unused_variabl) let x = 1; 0 }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("unrecognized warning name 'unused_variabl'"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_copy_directive_on_function_rejected() {
        let err = parse("@copy\nfn main() -> i32 { 0 }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("@copy can only be applied to structs"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_copy_directive_on_let_rejected() {
        let err = parse("fn main() -> i32 { @copy let x = 1; x }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("@copy can only be applied to structs"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_duplicate_comptime_modifier_rejected() {
        // RUE-133: `comptime comptime T: type` used to parse, with the second
        // `comptime` landing in the (now removed) ParamMode::Comptime duality.
        let err = parse("fn id(comptime comptime T: type, x: T) -> T { x }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("duplicate parameter modifier 'comptime'"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_conflicting_param_modifiers_rejected() {
        let err = parse("fn f(comptime inout x: i32) -> i32 { x }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("conflicting parameter modifiers 'comptime' and 'inout'"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_as_cast_targeted_error() {
        // RUE-133: `5 as i64` used to die with a generic "expected semicolon".
        let err = parse("fn main() -> i32 { let x = 5 as i64; 0 }").unwrap_err();
        let msg = format!("{}", err.first().expect("expected at least one error"));
        assert!(
            msg.contains("Rue has no 'as' cast operator"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn test_as_still_usable_as_identifier() {
        // `as` is not a keyword; only `expr as ...` is special-cased.
        let result = parse("fn main() -> i32 { let as = 3; as + 2 }");
        assert!(result.is_ok(), "{:?}", result.err());
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
    fn test_try_postfix_on_call() {
        // `foo()?` parses as Try wrapping a Call (RUE-6).
        let result = parse_expr("foo()?").unwrap();
        match &result.expr {
            Expr::Try(try_expr) => match try_expr.operand.as_ref() {
                Expr::Call(_) => {}
                other => panic!("expected Call operand, got {:?}", other),
            },
            other => panic!("expected Try, got {:?}", other),
        }
    }

    #[test]
    fn test_try_postfix_chains_with_field() {
        // `x?.field` — `?` binds to `x`, then `.field` applies to the result.
        let result = parse_expr("x?.field").unwrap();
        match &result.expr {
            Expr::Field(field) => {
                assert_eq!(result.get(field.field.name), "field");
                match field.base.as_ref() {
                    Expr::Try(try_expr) => match try_expr.operand.as_ref() {
                        Expr::Ident(ident) => assert_eq!(result.get(ident.name), "x"),
                        other => panic!("expected Ident operand, got {:?}", other),
                    },
                    other => panic!("expected Try base, got {:?}", other),
                }
            }
            other => panic!("expected Field, got {:?}", other),
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
        // Associated functions are called with `.` (RUE-488): `Type.function(args)`
        // parses as a method call whose receiver is the type name; sema
        // reinterprets it as an associated-function call.
        let result = parse_expr("Point.new(1, 2)").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                match call.receiver.as_ref() {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "Point"),
                    other => panic!("expected Ident receiver, got {other:?}"),
                }
                assert_eq!(result.get(call.method.name), "new");
                assert_eq!(call.args.len(), 2);
            }
            _ => panic!("expected MethodCall, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_path_separator_removed() {
        // `::` is no longer a path/member-access operator (RUE-488); it is a
        // hard parse error whose help points at `.`.
        let err = parse("enum E { A } fn main() -> i32 { let x = E::A; 0 }").unwrap_err();
        assert!(
            err.iter().any(|e| {
                e.diagnostic()
                    .helps
                    .iter()
                    .any(|h| h.0.contains("use `.` for member access"))
            }),
            "expected a `::`-removed help pointing at `.`, got {err:?}"
        );
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
                    TypeExpr::Array { length, .. } => {
                        assert_eq!(*length, ArrayLength::Literal(3))
                    }
                    _ => panic!("expected Array type"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_slice_type_param_parses() {
        // ADR-0043 / RUE-322: `[T]` (no length) parses to TypeExpr::Slice; the
        // `borrow` mode is carried separately on the parameter.
        let result = parse("fn sum(borrow s: [i32]) -> i32 { 0 }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].mode, ParamMode::Borrow);
                match &f.params[0].ty {
                    TypeExpr::Slice { element, .. } => match element.as_ref() {
                        TypeExpr::Named(ident) => assert_eq!(result.get(ident.name), "i32"),
                        other => panic!("expected named element, got {other:?}"),
                    },
                    other => panic!("expected Slice type, got {other:?}"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_array_vs_slice_type_distinct() {
        // `[i32; 3]` remains an Array; only the length-less `[i32]` is a Slice.
        let result = parse("fn f(borrow a: [i32; 3]) -> i32 { 0 }").unwrap();
        let Item::Function(func) = &result.ast.items[0] else {
            panic!("expected Function");
        };
        assert!(matches!(func.params[0].ty, TypeExpr::Array { .. }));
    }

    #[test]
    fn test_array_length_call_form() {
        // RUE-309: a comptime-evaluable call is accepted in array-length
        // position and parses to `ArrayLength::Call`, with a nested call as an
        // argument (`[i32; dbl(g(1))]`).
        let result = parse("fn f() { let a: [i32; dbl(g(1))] = x; }").unwrap();
        let Item::Function(func) = &result.ast.items[0] else {
            panic!("expected Function");
        };
        // Dig into the `let` statement's declared type.
        let Expr::Block(block) = &func.body else {
            panic!("expected block body");
        };
        let Statement::Let(let_stmt) = &block.statements[0] else {
            panic!("expected Let statement");
        };
        let Some(TypeExpr::Array { length, .. }) = &let_stmt.ty else {
            panic!("expected array type annotation");
        };
        let ArrayLength::Call { name, args } = length else {
            panic!("expected call-form array length, got {length:?}");
        };
        assert_eq!(result.get(name.name), "dbl");
        assert_eq!(args.len(), 1);
        let ArrayLength::Call {
            name: inner_name,
            args: inner_args,
        } = &args[0]
        else {
            panic!("expected nested call argument");
        };
        assert_eq!(result.get(inner_name.name), "g");
        assert_eq!(inner_args, &[ArrayLength::Literal(1)]);
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
        // Borrow argument in an associated-function call, spelled with `.`
        // (RUE-488): `Foo.bar(borrow x)` parses as a method call.
        let result = parse_expr("Foo.bar(borrow x)").unwrap();
        match &result.expr {
            Expr::MethodCall(call) => {
                assert_eq!(call.args.len(), 1);
                assert_eq!(call.args[0].mode, ArgMode::Borrow);
            }
            _ => panic!("expected MethodCall"),
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
    fn test_expression_position_expects_expression() {
        // RUE-19: atom-parser failures used to bubble up chumsky's catch-all
        // "expected something else". Expression positions should name the
        // syntactic context instead.
        let lines = error_lines("fn main() -> i32 { let x = ; }");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("expected expression, found ';'")),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains("something else")),
            "{lines:?}"
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
        // Normal spans should convert without issue and carry the file ID
        let simple = SimpleSpan::new(10, 20);
        let file = FileId::new(3);
        let rue = to_rue_span_with_file(simple, file);
        assert_eq!(rue.start, 10);
        assert_eq!(rue.end, 20);
        assert_eq!(rue.file_id, file);
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

    // ==================== Qualified Struct Literal Tests ====================

    #[test]
    fn test_qualified_struct_literal() {
        // module.Point { x: 1, y: 2 } should parse as a qualified struct literal
        let result = parse_expr("mod.Point { x: 1, y: 2 }").unwrap();
        match &result.expr {
            Expr::StructLit(lit) => {
                // Verify it has a base (the module)
                assert!(
                    lit.base.is_some(),
                    "qualified struct literal should have a base"
                );
                match lit.base.as_ref().unwrap().as_ref() {
                    Expr::Ident(ident) => assert_eq!(result.get(ident.name), "mod"),
                    _ => panic!("base should be Ident, got {:?}", lit.base),
                }
                // Verify struct name
                assert_eq!(result.get(lit.name.name), "Point");
                // Verify fields
                assert_eq!(lit.fields.len(), 2);
                assert_eq!(result.get(lit.fields[0].name.name), "x");
                assert_eq!(result.get(lit.fields[1].name.name), "y");
            }
            _ => panic!("expected StructLit, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_qualified_struct_literal_empty() {
        // module.Empty {} should parse as a qualified struct literal with no fields
        let result = parse_expr("mod.Empty {}").unwrap();
        match &result.expr {
            Expr::StructLit(lit) => {
                assert!(
                    lit.base.is_some(),
                    "qualified struct literal should have a base"
                );
                assert_eq!(result.get(lit.name.name), "Empty");
                assert_eq!(lit.fields.len(), 0);
            }
            _ => panic!("expected StructLit, got {:?}", result.expr),
        }
    }

    #[test]
    fn test_field_access_then_block() {
        // obj.field; { 1 } should parse as field access (discarded) followed by block expression
        // Note: obj.field { 1 } (without semicolon) is a syntax error because there's no
        // operator between the field access and the block.
        let result = parse("fn main() -> i32 { x.field; { 1 } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => {
                    // The block should have a statement (field access discarded) and
                    // a final expression (the inner block returning 1)
                    assert_eq!(block.statements.len(), 1);
                    match &block.statements[0] {
                        Statement::Expr(Expr::Field(field)) => {
                            assert_eq!(result.get(field.field.name), "field");
                        }
                        _ => panic!("expected Field statement, got {:?}", block.statements[0]),
                    }
                    match block.expr.as_ref() {
                        Expr::Block(inner) => match inner.expr.as_ref() {
                            Expr::Int(lit) => assert_eq!(lit.value, 1),
                            _ => panic!("expected Int, got {:?}", inner.expr),
                        },
                        _ => panic!("expected Block, got {:?}", block.expr),
                    }
                }
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_field_access_block_without_semicolon_is_error() {
        // obj.field { 1 } without semicolon is a syntax error - it's not a struct literal
        // (because { 1 } doesn't match the { ident: } pattern) and it's not valid syntax.
        let result = parse("fn main() -> i32 { x.field { 1 } }");
        assert!(
            result.is_err(),
            "field + block without semicolon should be syntax error"
        );
    }

    #[test]
    fn test_chained_qualified_struct_literal() {
        // a.b.Point { x: 1 } - nested field access then struct literal
        let result = parse_expr("a.b.Point { x: 1 }").unwrap();
        match &result.expr {
            Expr::StructLit(lit) => {
                // Should have a base that's a.b
                assert!(lit.base.is_some());
                match lit.base.as_ref().unwrap().as_ref() {
                    Expr::Field(field) => {
                        assert_eq!(result.get(field.field.name), "b");
                        match field.base.as_ref() {
                            Expr::Ident(ident) => assert_eq!(result.get(ident.name), "a"),
                            _ => panic!("inner base should be Ident"),
                        }
                    }
                    _ => panic!("base should be Field, got {:?}", lit.base),
                }
                assert_eq!(result.get(lit.name.name), "Point");
            }
            _ => panic!("expected StructLit, got {:?}", result.expr),
        }
    }

    // ==================== Anonymous Struct Method Parsing Tests ====================

    #[test]
    fn test_anon_enum_type_expr() {
        // Anonymous enum type as a comptime type-function body: variant names
        // plus a tuple payload referring to a comptime type parameter (RUE-6
        // phase 2).
        let result =
            parse("fn Option(comptime T: type) -> type { enum { Some(T), None } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(result.get(f.name.name), "Option");
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                            TypeExpr::AnonymousEnum { variants, .. } => {
                                assert_eq!(variants.len(), 2);
                                assert_eq!(result.get(variants[0].name.name), "Some");
                                assert_eq!(variants[0].payload.len(), 1);
                                assert_eq!(result.get(variants[1].name.name), "None");
                                assert!(variants[1].payload.is_empty());
                            }
                            _ => panic!("expected AnonymousEnum"),
                        },
                        _ => panic!("expected TypeLit"),
                    },
                    _ => panic!("expected Block"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_type_call_in_return_position() {
        // A comptime type-function application used directly as a return type
        // (RUE-241): `-> Result(i32, i32)`. The type grammar now accepts a
        // `Name(args...)` call in type position.
        let result = parse("fn divide(a: i32, b: i32) -> Result(i32, i32) { 0 }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match f.return_type.as_ref().expect("return type") {
                TypeExpr::TypeCall { name, args, .. } => {
                    assert_eq!(result.get(name.name), "Result");
                    assert_eq!(args.len(), 2);
                    match (&args[0], &args[1]) {
                        (TypeExpr::Named(a), TypeExpr::Named(b)) => {
                            assert_eq!(result.get(a.name), "i32");
                            assert_eq!(result.get(b.name), "i32");
                        }
                        _ => panic!("expected two Named type args"),
                    }
                }
                other => panic!("expected TypeCall, got {other:?}"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_type_call_nested_in_param_position() {
        // Nested type-function calls compose in type position, here as a
        // parameter type: `r: Result(Option(i32), i32)` (RUE-241).
        let result = parse("fn f(r: Result(Option(i32), i32)) -> i32 { 0 }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.params[0].ty {
                TypeExpr::TypeCall { name, args, .. } => {
                    assert_eq!(result.get(name.name), "Result");
                    assert_eq!(args.len(), 2);
                    match &args[0] {
                        TypeExpr::TypeCall {
                            name: inner_name,
                            args: inner_args,
                            ..
                        } => {
                            assert_eq!(result.get(inner_name.name), "Option");
                            assert_eq!(inner_args.len(), 1);
                        }
                        _ => panic!("expected nested TypeCall as first arg"),
                    }
                }
                other => panic!("expected TypeCall param type, got {other:?}"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_fields_only() {
        // Anonymous struct with only fields (no methods)
        let result = parse("fn make_type() -> type { struct { x: i32, y: i32 } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => {
                assert_eq!(result.get(f.name.name), "make_type");
                match &f.body {
                    Expr::Block(block) => match block.expr.as_ref() {
                        Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                            TypeExpr::AnonymousStruct {
                                fields, methods, ..
                            } => {
                                assert_eq!(fields.len(), 2);
                                assert_eq!(result.get(fields[0].name.name), "x");
                                assert_eq!(result.get(fields[1].name.name), "y");
                                assert!(methods.is_empty());
                            }
                            _ => panic!("expected AnonymousStruct"),
                        },
                        _ => panic!("expected TypeLit"),
                    },
                    _ => panic!("expected Block"),
                }
            }
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_method() {
        // Anonymous struct with a single method
        let result =
            parse("fn make_type() -> type { struct { x: i32, fn get_x(self) -> i32 { self.x } } }")
                .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert_eq!(fields.len(), 1);
                            assert_eq!(result.get(fields[0].name.name), "x");
                            assert_eq!(methods.len(), 1);
                            assert_eq!(result.get(methods[0].name.name), "get_x");
                            assert!(
                                methods[0].receiver.is_some(),
                                "method should have self receiver"
                            );
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_associated_function() {
        // Anonymous struct with an associated function (no self)
        let result = parse(
            "fn make_type() -> type { struct { x: i32, fn new() -> Self { Self { x: 0 } } } }",
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert_eq!(fields.len(), 1);
                            assert_eq!(methods.len(), 1);
                            assert_eq!(result.get(methods[0].name.name), "new");
                            assert!(
                                methods[0].receiver.is_none(),
                                "associated function should not have self"
                            );
                            // Check return type is Self
                            match &methods[0].return_type {
                                Some(TypeExpr::Named(ident)) => {
                                    assert_eq!(result.get(ident.name), "Self");
                                }
                                _ => panic!("expected Self return type"),
                            }
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_with_multiple_methods() {
        // Anonymous struct with multiple methods
        let result = parse(
            r#"
            fn make_type() -> type {
                struct {
                    value: i32,
                    fn get(self) -> i32 { self.value }
                    fn set(self, v: i32) -> Self { Self { value: v } }
                }
            }
        "#,
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert_eq!(fields.len(), 1);
                            assert_eq!(result.get(fields[0].name.name), "value");
                            assert_eq!(methods.len(), 2);
                            assert_eq!(result.get(methods[0].name.name), "get");
                            assert_eq!(result.get(methods[1].name.name), "set");
                            // Check set has a parameter
                            assert_eq!(methods[1].params.len(), 1);
                            assert_eq!(result.get(methods[1].params[0].name.name), "v");
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_anon_struct_methods_only() {
        // Anonymous struct with only methods (no fields)
        let result =
            parse("fn make_type() -> type { struct { fn new() -> Self { Self { } } } }").unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct {
                            fields, methods, ..
                        } => {
                            assert!(fields.is_empty());
                            assert_eq!(methods.len(), 1);
                            assert_eq!(result.get(methods[0].name.name), "new");
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_self_type_in_return() {
        // Test Self as return type
        let result =
            parse("fn make_type() -> type { struct { x: i32, fn clone(self) -> Self { self } } }")
                .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct { methods, .. } => {
                            match &methods[0].return_type {
                                Some(TypeExpr::Named(ident)) => {
                                    assert_eq!(result.get(ident.name), "Self");
                                }
                                _ => panic!("expected Self return type"),
                            }
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_self_type_in_param() {
        // Test Self as parameter type
        let result = parse(
            "fn make_type() -> type { struct { fn combine(self, other: Self) -> Self { self } } }",
        )
        .unwrap();
        match &result.ast.items[0] {
            Item::Function(f) => match &f.body {
                Expr::Block(block) => match block.expr.as_ref() {
                    Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                        TypeExpr::AnonymousStruct { methods, .. } => {
                            // Check parameter type is Self
                            let param = &methods[0].params[0];
                            match &param.ty {
                                TypeExpr::Named(ident) => {
                                    assert_eq!(result.get(ident.name), "Self");
                                }
                                _ => panic!("expected Self param type"),
                            }
                        }
                        _ => panic!("expected AnonymousStruct"),
                    },
                    _ => panic!("expected TypeLit"),
                },
                _ => panic!("expected Block"),
            },
            _ => panic!("expected Function"),
        }
    }

    #[test]
    fn test_self_type_standalone() {
        // Test Self as a standalone type (e.g., in struct method context)
        // This just tests lexing/parsing of Self as a type keyword
        let result = parse("struct Foo { x: i32, fn clone(self) -> Self { self } }").unwrap();
        match &result.ast.items[0] {
            Item::Struct(struct_decl) => {
                let method = &struct_decl.methods[0];
                match &method.return_type {
                    Some(TypeExpr::Named(ident)) => {
                        assert_eq!(result.get(ident.name), "Self");
                    }
                    _ => panic!("expected Self return type"),
                }
            }
            _ => panic!("expected Struct"),
        }
    }

    // --- Nesting-depth guard (RUE-42) -------------------------------------

    /// Parse a source string and return the error code of the first error, if
    /// any. Used by the nesting-depth tests.
    fn first_error_code(source: &str) -> Option<rue_error::ErrorCode> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().ok()?;
        let parser = ChumskyParser::new(tokens, interner);
        match parser.parse() {
            Ok(_) => None,
            Err(errs) => errs.first().map(|e| e.kind.code()),
        }
    }

    #[test]
    fn test_deeply_nested_parens_rejected_cleanly() {
        // Far past the limit: must be a clean E0482, never a stack overflow.
        let n = MAX_NESTING_DEPTH + 50;
        let src = format!(
            "fn main() -> i32 {{ {}42{} }}",
            "(".repeat(n),
            ")".repeat(n)
        );
        assert_eq!(
            first_error_code(&src),
            Some(rue_error::ErrorCode::NESTING_LIMIT_EXCEEDED)
        );
    }

    #[test]
    fn test_deep_operator_chain_rejected_cleanly() {
        // `1 + 1 + ... + 1` is built iteratively by the Pratt parser, so it
        // does not overflow the parser — but it would overflow AstGen / the
        // recursive Drop of the AST. The pre-scan must still reject it.
        let src = format!(
            "fn main() -> i32 {{ {}1 }}",
            "1 + ".repeat(MAX_NESTING_DEPTH + 50)
        );
        assert_eq!(
            first_error_code(&src),
            Some(rue_error::ErrorCode::NESTING_LIMIT_EXCEEDED)
        );
    }

    #[test]
    fn test_deep_field_chain_rejected_cleanly() {
        let src = format!(
            "fn main() -> i32 {{ x{} }}",
            ".f".repeat(MAX_NESTING_DEPTH + 50)
        );
        assert_eq!(
            first_error_code(&src),
            Some(rue_error::ErrorCode::NESTING_LIMIT_EXCEEDED)
        );
    }

    #[test]
    fn test_deep_else_if_chain_rejected_cleanly() {
        let src = format!(
            "fn main() -> i32 {{ {} {{ 0 }} }}",
            "if true { 1 } else ".repeat(MAX_NESTING_DEPTH + 50)
        );
        assert_eq!(
            first_error_code(&src),
            Some(rue_error::ErrorCode::NESTING_LIMIT_EXCEEDED)
        );
    }

    #[test]
    fn test_moderate_nesting_still_parses() {
        // Well within the limit: parsing must succeed (regression guard against
        // the pre-scan being too aggressive, and proof the large parser stack
        // is in effect).
        let n = 100;
        let src = format!(
            "fn main() -> i32 {{ {}42{} }}",
            "(".repeat(n),
            ")".repeat(n)
        );
        assert!(first_error_code(&src).is_none());
    }

    #[test]
    fn test_nested_array_type_matches_paren_limit() {
        // RUE-533: nested array types used to be counted TWICE per level, so
        // the nesting guard rejected them at ~128 levels while parens reached
        // ~256 — a non-uniform limit the spec (Appendix C) forbids. The bound
        // must apply uniformly, so an array type and a parenthesised
        // expression in the same enclosing context accept/reject at the same
        // depth.
        let array_type = |n: usize| {
            format!(
                "fn f(x: {}i32{}) {{}}\nfn main() -> i32 {{ 0 }}\n",
                "[".repeat(n),
                "; 1]".repeat(n)
            )
        };
        let param_paren = |n: usize| {
            // A paren-grouped default-ish expression in the same param position
            // shape: use a body expression instead, matched depth.
            format!(
                "fn main() -> i32 {{ {}0{} }}\n",
                "(".repeat(n),
                ")".repeat(n)
            )
        };

        // A depth that previously tripped the halved array limit (128) now
        // parses cleanly, proving the double-count is gone.
        assert!(
            first_error_code(&array_type(200)).is_none(),
            "200-level array type must parse (was rejected at 128 pre-fix)"
        );

        // Array types and parens share the same accept/reject boundary now.
        // Find the paren boundary, then assert the array type agrees at it.
        let mut boundary = 0;
        for n in 250..=257 {
            if first_error_code(&param_paren(n)).is_none() {
                boundary = n;
            } else {
                break;
            }
        }
        assert!(
            (255..=256).contains(&boundary),
            "unexpected paren boundary {boundary}"
        );
        assert!(
            first_error_code(&array_type(boundary)).is_none(),
            "array type must parse at the same depth parens do ({boundary})"
        );
        assert_eq!(
            first_error_code(&array_type(boundary + 1)).map(|c| c.0),
            Some(482),
            "one past the boundary must be a clean E0482 for array types too"
        );
    }

    #[test]
    fn test_deep_array_type_rejected_cleanly_not_overflow() {
        // Far past the limit stays a clean E0482 (parse guard), never a stack
        // overflow through parse / astgen / drop (RUE-533 / RUE-42).
        let n = MAX_NESTING_DEPTH + 200;
        let src = format!(
            "fn f(x: {}i32{}) {{}}\nfn main() -> i32 {{ 0 }}\n",
            "[".repeat(n),
            "; 1]".repeat(n)
        );
        assert_eq!(first_error_code(&src).map(|c| c.0), Some(482));
    }

    /// Collect the rendered `[code] message` lines for a source that must fail
    /// to parse. Panics if the source parses cleanly.
    fn error_lines(source: &str) -> Vec<String> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().expect("tokenize");
        let parser = ChumskyParser::new(tokens, interner);
        let errs = parser.parse().expect_err("expected a parse error");
        errs.iter().map(|e| e.kind.to_string()).collect()
    }

    #[test]
    fn test_self_in_non_receiver_position_reports_single_error() {
        // RUE-19: `self` after a regular parameter used to produce the identical
        // E0100 twice (both the self-param and the regular-param branch failed
        // at the same token). Duplicates must be collapsed to one diagnostic.
        let source = "struct Foo { fn bar(x: i32, self) -> i32 { x } }";
        let lines = error_lines(source);
        assert_eq!(
            lines.len(),
            1,
            "duplicate parse errors not deduped: {lines:?}"
        );
        assert!(lines[0].contains("found 'self'"), "{lines:?}");

        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().expect("tokenize");
        let parser = ChumskyParser::new(tokens, interner);
        let errs = parser.parse().expect_err("expected a parse error");
        let help = errs
            .first()
            .expect("expected a parse error")
            .diagnostic()
            .helps
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            help.iter()
                .any(|h| h.contains("methods take `self` as the first parameter")),
            "{help:?}"
        );
    }

    #[test]
    fn test_type_position_expects_type() {
        // RUE-19: a bad token in type position summarizes as "expected type"
        // rather than enumerating the raw first tokens (`'(' or '!' or '['`).
        let lines = error_lines("fn foo() -> 123 { 0 }");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("expected type, found integer")),
            "{lines:?}"
        );
    }

    #[test]
    fn test_empty_array_length_expects_array_length() {
        // RUE-19: the array-length `select!` catch-all used to render the bare
        // "expected something else"; it now names the array-length position.
        let lines = error_lines("fn foo(a: [i32; ]) -> i32 { 0 }");
        assert!(
            lines.iter().any(|l| l.contains("expected array length")),
            "{lines:?}"
        );
    }
}
