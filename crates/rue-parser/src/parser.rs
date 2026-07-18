//! Handwritten recursive-descent parser with Pratt expression parsing.

use crate::ast::*;
use crate::parser_policy::{condition, diagnostics, nesting, recovery};
use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, ErrorKind, MultiErrorResult};
use rue_lexer::{Token, TokenKind};
use rue_span::{FileId, Span};
use tracing::{info, info_span};

type PResult<T> = Result<T, ()>;

/// Rue's token-cursor recursive-descent parser.
pub struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    interner: ThreadedRodeo,
    syms: PrimitiveTypeSpurs,
    file_id: FileId,
    errors: Vec<CompileError>,
}

struct PrimitiveTypeSpurs {
    i8: Spur,
    i16: Spur,
    i32: Spur,
    i64: Spur,
    u8: Spur,
    u16: Spur,
    u32: Spur,
    u64: Spur,
    bool: Spur,
    self_type: Spur,
    self_value: Spur,
    type_kw: Spur,
    as_kw: Spur,
    underscore: Spur,
    drop_kw: Spur,
    drop_marker: Spur,
    allow_directive: Spur,
    copy_directive: Spur,
}

impl PrimitiveTypeSpurs {
    fn new(interner: &mut ThreadedRodeo) -> Self {
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
            self_value: interner.get_or_intern("self"),
            type_kw: interner.get_or_intern("type"),
            as_kw: interner.get_or_intern("as"),
            drop_kw: interner.get_or_intern("drop"),
            drop_marker: interner.get_or_intern("__drop"),
            allow_directive: interner.get_or_intern("allow"),
            copy_directive: interner.get_or_intern("copy"),
            underscore: interner.get_or_intern("_"),
        }
    }
}

impl Parser {
    /// Create a parser from lexer tokens and their shared symbol interner.
    pub fn new(tokens: Vec<Token>, mut interner: ThreadedRodeo) -> Self {
        let file_id = tokens.first().map(|t| t.span.file_id).unwrap_or_default();
        let syms = {
            let _span = info_span!("parser_state_setup").entered();
            PrimitiveTypeSpurs::new(&mut interner)
        };
        Self {
            tokens,
            cursor: 0,
            interner,
            syms,
            file_id,
            errors: Vec::new(),
        }
    }

    /// Parse into an AST, returning all parser diagnostics on failure.
    pub fn parse(self) -> MultiErrorResult<(Ast, ThreadedRodeo)> {
        self.parse_preserving_interner()
            .map_err(|(errors, _interner)| errors)
    }

    /// Parse while retaining the shared interner when this file is malformed.
    pub fn parse_preserving_interner(
        mut self,
    ) -> Result<(Ast, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        let input_token_count = self.tokens.len();
        let parser_token_count = self
            .tokens
            .iter()
            .filter(|token| token.kind != TokenKind::Eof)
            .count();
        let nesting_error = {
            let _span = info_span!("parser_nesting_scan").entered();
            nesting::check_nesting_depth(&self.tokens)
        };
        if let Some(error) = nesting_error {
            info!(
                outcome = "nesting_error",
                input_token_count,
                parser_token_count,
                ast_item_count = 0,
                raw_parse_error_count = 0,
                parse_error_count = 0,
                validation_error_count = 0,
                "parser complete"
            );
            return Err((CompileErrors::from(vec![error]), self.interner));
        }

        let items = {
            let _span = info_span!("parser_grammar_execution").entered();
            let mut items = Vec::new();
            while !self.at(TokenKind::Eof) {
                let start = self.cursor;
                match self.item() {
                    Ok(item) => items.push(item),
                    Err(()) => {
                        self.cursor = start;
                        let span = self.recover_item();
                        items.push(Item::Error(span));
                    }
                }
            }
            items
        };
        let raw_parse_error_count = self.errors.len();
        self.remove_subsumed_identifier_errors();
        diagnostics::dedupe_parse_errors(&mut self.errors);
        let parse_error_count = self.errors.len();
        if !self.errors.is_empty() {
            info!(
                outcome = "parse_error",
                input_token_count,
                parser_token_count,
                ast_item_count = 0,
                raw_parse_error_count,
                parse_error_count,
                validation_error_count = 0,
                "parser complete"
            );
            return Err((CompileErrors::from(self.errors), self.interner));
        }

        let ast = Ast { items };
        let validation = {
            let _span = info_span!("parser_directive_validation").entered();
            crate::validate::check_directives(&ast, &self.interner)
        };
        if !validation.is_empty() {
            info!(
                outcome = "validation_error",
                input_token_count,
                parser_token_count,
                ast_item_count = ast.items.len(),
                raw_parse_error_count,
                parse_error_count,
                validation_error_count = validation.len(),
                "parser complete"
            );
            return Err((CompileErrors::from(validation), self.interner));
        }

        info!(
            outcome = "success",
            input_token_count,
            parser_token_count,
            ast_item_count = ast.items.len(),
            raw_parse_error_count,
            parse_error_count,
            validation_error_count = 0,
            "parser complete"
        );
        Ok((ast, self.interner))
    }

    fn kind(&self) -> TokenKind {
        self.tokens
            .get(self.cursor)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }
    fn nth(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.cursor + n)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }
    fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind
    }
    fn bump(&mut self) -> Token {
        let token = self.tokens.get(self.cursor).cloned().unwrap_or(Token {
            kind: TokenKind::Eof,
            span: Span::point_in_file(self.file_id, self.end_offset()),
        });
        if token.kind != TokenKind::Eof {
            self.cursor += 1;
        }
        token
    }
    fn end_offset(&self) -> u32 {
        self.tokens.last().map(|t| t.span.end).unwrap_or(0)
    }
    fn start(&self) -> u32 {
        self.tokens
            .get(self.cursor)
            .map(|t| t.span.start)
            .unwrap_or(self.end_offset())
    }
    fn previous_end(&self) -> u32 {
        self.cursor
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map(|t| t.span.end)
            .unwrap_or(self.start())
    }
    fn span_from(&self, start: u32) -> Span {
        Span::with_file(self.file_id, start, self.previous_end())
    }
    fn error(&mut self, message: impl Into<String>) {
        let span = self
            .tokens
            .get(self.cursor)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
        self.errors.push(CompileError::new(
            ErrorKind::ParseError(message.into()),
            span,
        ));
    }
    fn error_at(&mut self, message: impl Into<String>, span: Span) {
        self.errors.push(CompileError::new(
            ErrorKind::ParseError(message.into()),
            span,
        ));
    }
    fn unexpected(&mut self, expected: impl Into<String>) {
        let expected = expected.into();
        let found_kind = self.kind();
        let span = self
            .tokens
            .get(self.cursor)
            .map(|token| token.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
        if expected == "identifier"
            && self.errors.iter().any(|error| {
                error.span() == Some(span)
                    && matches!(
                        &error.kind,
                        ErrorKind::UnexpectedToken { expected, found }
                            if expected == "identifier or 'drop'" && found == found_kind.name()
                    )
            })
        {
            return;
        }
        let mut error = CompileError::new(
            ErrorKind::UnexpectedToken {
                expected: expected.into(),
                found: found_kind.name().to_owned().into(),
            },
            span,
        );
        if found_kind == TokenKind::SelfValue {
            error = error.with_help("methods take `self` as the first parameter");
        } else if found_kind == TokenKind::Impl {
            error = error.with_help(
                "Rue has no `impl` blocks; define methods inside the struct body, \
                 e.g. `struct S { x: i32, fn m(self) -> i32 { self.x } }`",
            );
        } else if found_kind == TokenKind::ColonColon {
            error = error.with_help(
                "`::` is not a Rue operator; use `.` for member access, e.g. \
                 `Enum.Variant` or `Type.function()`",
            );
        }
        self.errors.push(error);
    }
    fn remove_subsumed_identifier_errors(&mut self) {
        let mut retained = Vec::with_capacity(self.errors.len());
        for error in std::mem::take(&mut self.errors) {
            let subsumed = matches!(
                &error.kind,
                ErrorKind::UnexpectedToken { expected, .. } if expected == "identifier"
            ) && retained.iter().any(|prior: &CompileError| {
                prior.span() == error.span()
                    && matches!(
                        &prior.kind,
                        ErrorKind::UnexpectedToken { expected, .. }
                            if expected == "identifier or 'drop'"
                    )
            });
            if !subsumed {
                retained.push(error);
            }
        }
        self.errors = retained;
    }
    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, kind: TokenKind) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            self.unexpected(kind.name());
            Err(())
        }
    }
    fn ident_expected(&mut self, expected: &'static str) -> PResult<Ident> {
        let token = self.bump();
        match token.kind {
            TokenKind::Ident(name) => Ok(Ident {
                name,
                span: token.span,
            }),
            _ => {
                self.cursor = self
                    .cursor
                    .saturating_sub((token.kind != TokenKind::Eof) as usize);
                self.unexpected(expected);
                Err(())
            }
        }
    }
    fn ident(&mut self) -> PResult<Ident> {
        self.ident_expected("identifier")
    }

    fn directives(&mut self) -> PResult<Directives> {
        let mut out = Directives::new();
        while self.at(TokenKind::At) {
            let start = self.bump().span.start;
            let name = self.ident()?;
            let mut args = Vec::new();
            if self.eat(TokenKind::LParen) {
                if !self.at(TokenKind::RParen) {
                    loop {
                        args.push(DirectiveArg::Ident(self.ident()?));
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(TokenKind::RParen) {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RParen)?;
            }
            out.push(Directive {
                name,
                args,
                span: self.span_from(start),
            });
        }
        Ok(out)
    }

    fn item(&mut self) -> PResult<Item> {
        let start = self.start();
        let directives = self.directives()?;
        let visibility = if self.eat(TokenKind::Pub) {
            Visibility::Public
        } else {
            Visibility::Private
        };
        match self.kind() {
            TokenKind::Unchecked | TokenKind::Fn => self
                .function(start, directives, visibility)
                .map(Item::Function),
            TokenKind::Linear | TokenKind::Struct => self
                .struct_decl(start, directives, visibility)
                .map(Item::Struct),
            TokenKind::Enum if directives.is_empty() => {
                self.enum_decl(start, visibility).map(Item::Enum)
            }
            TokenKind::Drop if directives.is_empty() && visibility == Visibility::Private => {
                self.drop_fn(start).map(Item::DropFn)
            }
            TokenKind::Const => self
                .const_decl(start, directives, visibility)
                .map(Item::Const),
            _ => {
                self.unexpected("'@' or 'pub' or 'unchecked' or 'fn' or …");
                Err(())
            }
        }
    }

    fn function(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<Function> {
        let is_unchecked = self.eat(TokenKind::Unchecked);
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        let params = self.params()?;
        let return_type = if self.eat(TokenKind::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        let body = Expr::Block(self.block()?);
        Ok(Function {
            directives,
            visibility,
            is_unchecked,
            name,
            params,
            return_type,
            body,
            span: self.span_from(start),
        })
    }

    fn struct_decl(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<StructDecl> {
        let is_linear = self.eat(TokenKind::Linear);
        self.expect(TokenKind::Struct)?;
        let name = self.ident()?;
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut saw_method = false;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Fn) || self.at(TokenKind::At) {
                saw_method = true;
                methods.push(self.method()?);
            } else {
                if saw_method {
                    self.error("struct fields must precede methods");
                    return Err(());
                }
                fields.push(self.field_decl()?);
                if !self.eat(TokenKind::Comma)
                    && !self.at(TokenKind::RBrace)
                    && !self.at(TokenKind::Fn)
                    && !self.at(TokenKind::At)
                {
                    self.error("expected ',' after struct field");
                    return Err(());
                }
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(StructDecl {
            directives,
            visibility,
            is_linear,
            name,
            fields,
            methods,
            span: self.span_from(start),
        })
    }

    fn enum_decl(&mut self, start: u32, visibility: Visibility) -> PResult<EnumDecl> {
        self.expect(TokenKind::Enum)?;
        let name = self.ident()?;
        let variants = self.enum_variants()?;
        Ok(EnumDecl {
            visibility,
            name,
            variants,
            span: self.span_from(start),
        })
    }

    fn enum_variants(&mut self) -> PResult<Vec<EnumVariant>> {
        self.expect(TokenKind::LBrace)?;
        let mut variants = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let start = self.start();
            let name = self.ident()?;
            let mut payload = Vec::new();
            if self.eat(TokenKind::LParen) {
                if self.at(TokenKind::RParen) {
                    self.error("expected type in enum payload");
                    return Err(());
                }
                loop {
                    payload.push(self.ty()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen)?;
            }
            variants.push(EnumVariant {
                name,
                payload,
                span: self.span_from(start),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(variants)
    }

    fn drop_fn(&mut self, start: u32) -> PResult<DropFn> {
        self.expect(TokenKind::Drop)?;
        self.expect(TokenKind::Fn)?;
        let type_name = self.ident()?;
        self.expect(TokenKind::LParen)?;
        let self_tok = self.expect(TokenKind::SelfValue)?;
        self.expect(TokenKind::RParen)?;
        let body = Expr::Block(self.block()?);
        Ok(DropFn {
            type_name,
            self_param: SelfParam {
                mode: ParamMode::Normal,
                is_mut: false,
                span: self_tok.span,
            },
            body,
            span: self.span_from(start),
        })
    }

    fn const_decl(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<ConstDecl> {
        self.expect(TokenKind::Const)?;
        let name = self.ident()?;
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.ty()?)
        } else {
            None
        };
        self.expect(TokenKind::Eq)?;
        let init = Box::new(self.expr()?);
        self.expect(TokenKind::Semi)?;
        Ok(ConstDecl {
            directives,
            visibility,
            name,
            ty,
            init,
            span: self.span_from(start),
        })
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect(TokenKind::LParen)?;
        let mut out = Vec::new();
        if !self.at(TokenKind::RParen) {
            loop {
                out.push(self.param()?);
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RParen) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(out)
    }

    fn param(&mut self) -> PResult<Param> {
        let start = self.start();
        let first_modifier = self.kind();
        let mode = match first_modifier {
            TokenKind::Comptime => {
                self.bump();
                ParamMode::Comptime
            }
            TokenKind::Inout => {
                self.bump();
                ParamMode::Inout
            }
            TokenKind::Borrow => {
                self.bump();
                ParamMode::Borrow
            }
            _ => ParamMode::Normal,
        };
        if matches!(
            self.kind(),
            TokenKind::Comptime | TokenKind::Inout | TokenKind::Borrow
        ) {
            let second = self.kind();
            let message = if first_modifier == second {
                format!("duplicate parameter modifier {}", second.name())
            } else {
                format!(
                    "conflicting parameter modifiers {} and {}; a parameter takes at most one of 'comptime', 'inout', or 'borrow'",
                    first_modifier.name(),
                    second.name()
                )
            };
            self.error(message);
            return Err(());
        }
        let name = self.ident_expected("'comptime' or 'inout' or 'borrow' or identifier or …")?;
        self.expect(TokenKind::Colon)?;
        let ty = self.ty()?;
        Ok(Param {
            mode,
            name,
            ty,
            span: self.span_from(start),
        })
    }

    fn field_decl(&mut self) -> PResult<FieldDecl> {
        let start = self.start();
        let name = self.ident_expected("identifier or '@' or 'fn' or '}'")?;
        self.expect(TokenKind::Colon)?;
        let ty = self.ty()?;
        Ok(FieldDecl {
            name,
            ty,
            span: self.span_from(start),
        })
    }

    fn ty(&mut self) -> PResult<TypeExpr> {
        let start = self.start();
        match self.kind() {
            TokenKind::LParen => {
                self.bump();
                self.expect(TokenKind::RParen)?;
                Ok(TypeExpr::Unit(self.span_from(start)))
            }
            TokenKind::Bang => {
                self.bump();
                Ok(TypeExpr::Never(self.span_from(start)))
            }
            TokenKind::LBracket => {
                self.bump();
                let element = Box::new(self.ty()?);
                let length = if self.eat(TokenKind::Semi) {
                    Some(self.array_length()?)
                } else {
                    None
                };
                self.expect(TokenKind::RBracket)?;
                let span = self.span_from(start);
                Ok(match length {
                    Some(length) => TypeExpr::Array {
                        element,
                        length,
                        span,
                    },
                    None => TypeExpr::Slice { element, span },
                })
            }
            TokenKind::Ptr => {
                self.bump();
                let mutable = if self.eat(TokenKind::Const) {
                    false
                } else if self.eat(TokenKind::Mut) {
                    true
                } else {
                    self.error("expected 'const' or 'mut' after 'ptr'");
                    return Err(());
                };
                let pointee = Box::new(self.ty()?);
                let span = self.span_from(start);
                Ok(if mutable {
                    TypeExpr::PointerMut { pointee, span }
                } else {
                    TypeExpr::PointerConst { pointee, span }
                })
            }
            TokenKind::Struct => self.anonymous_struct_type(false),
            TokenKind::Enum => {
                self.bump();
                let variants = self.enum_variants()?;
                Ok(TypeExpr::AnonymousEnum {
                    variants,
                    span: self.span_from(start),
                })
            }
            kind if self.primitive_spur(kind).is_some() => {
                let token = self.bump();
                Ok(TypeExpr::Named(Ident {
                    name: self.primitive_spur(token.kind).unwrap(),
                    span: token.span,
                }))
            }
            TokenKind::SelfType => {
                let token = self.bump();
                Ok(TypeExpr::Named(Ident {
                    name: self.syms.self_type,
                    span: token.span,
                }))
            }
            TokenKind::Type => {
                let token = self.bump();
                Ok(TypeExpr::Named(Ident {
                    name: self.syms.type_kw,
                    span: token.span,
                }))
            }
            TokenKind::Ident(_) => self.named_type(),
            _ => {
                self.unexpected("type");
                Err(())
            }
        }
    }

    fn primitive_spur(&self, kind: TokenKind) -> Option<Spur> {
        Some(match kind {
            TokenKind::I8 => self.syms.i8,
            TokenKind::I16 => self.syms.i16,
            TokenKind::I32 => self.syms.i32,
            TokenKind::I64 => self.syms.i64,
            TokenKind::U8 => self.syms.u8,
            TokenKind::U16 => self.syms.u16,
            TokenKind::U32 => self.syms.u32,
            TokenKind::U64 => self.syms.u64,
            TokenKind::Bool => self.syms.bool,
            _ => return None,
        })
    }

    fn named_type(&mut self) -> PResult<TypeExpr> {
        let start = self.start();
        let mut segments = vec![self.ident()?];
        while self.eat(TokenKind::Dot) {
            segments.push(self.ident()?);
        }
        if self.eat(TokenKind::LParen) {
            let mut args = Vec::new();
            if !self.at(TokenKind::RParen) {
                loop {
                    if self.at(TokenKind::Minus) || matches!(self.kind(), TokenKind::Int(_)) {
                        let arg_start = self.start();
                        let neg = self.eat(TokenKind::Minus);
                        if let TokenKind::Int(value) = self.bump().kind {
                            args.push(TypeExpr::IntArg {
                                value: if neg { -(value as i128) } else { value as i128 },
                                span: self.span_from(arg_start),
                            });
                        } else {
                            self.error("expected integer type argument");
                            return Err(());
                        }
                    } else {
                        args.push(self.ty()?);
                    }
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen)?;
            let span = self.span_from(start);
            if segments.len() == 1 {
                let name = segments[0];
                Ok(TypeExpr::TypeCall { name, args, span })
            } else {
                Ok(TypeExpr::QualifiedTypeCall {
                    segments,
                    args,
                    span,
                })
            }
        } else if segments.len() == 1 {
            Ok(TypeExpr::Named(segments[0]))
        } else {
            Ok(TypeExpr::Qualified {
                segments,
                span: self.span_from(start),
            })
        }
    }

    fn anonymous_struct_type(&mut self, allow_methods: bool) -> PResult<TypeExpr> {
        let start = self.start();
        self.expect(TokenKind::Struct)?;
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut methods_started = false;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Fn) || self.at(TokenKind::At) {
                if !allow_methods {
                    self.error("methods are not allowed in a type-position anonymous struct");
                    return Err(());
                }
                methods_started = true;
                methods.push(self.method()?);
            } else if self.at(TokenKind::Drop) {
                if !allow_methods {
                    self.error("methods are not allowed in a type-position anonymous struct");
                    return Err(());
                }
                methods_started = true;
                methods.push(self.anonymous_drop_method()?);
            } else {
                if methods_started {
                    self.error("struct fields must precede methods");
                    return Err(());
                }
                let fs = self.start();
                let name = self.ident()?;
                self.expect(TokenKind::Colon)?;
                let ty = self.ty()?;
                fields.push(AnonStructField {
                    name,
                    ty,
                    span: self.span_from(fs),
                });
                if !self.eat(TokenKind::Comma)
                    && !self.at(TokenKind::RBrace)
                    && !self.at(TokenKind::Fn)
                    && !self.at(TokenKind::At)
                    && !self.at(TokenKind::Drop)
                {
                    self.error("expected ',' after struct field");
                    return Err(());
                }
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(TypeExpr::AnonymousStruct {
            fields,
            methods,
            span: self.span_from(start),
        })
    }

    fn method(&mut self) -> PResult<Method> {
        let start = self.start();
        let directives = self.directives()?;
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        self.expect(TokenKind::LParen)?;
        let mut receiver = None;
        let mut params = Vec::new();
        if !self.at(TokenKind::RParen) {
            let checkpoint = self.cursor;
            // Receiver modifiers are mutually exclusive: `inout self`,
            // `borrow self`, or `mut self` (a by-value receiver that binds
            // mutably in the body). If `self` doesn't follow, the cursor is
            // reset and the tokens re-parse as an ordinary parameter list.
            let (mode, is_mut) = if self.eat(TokenKind::Inout) {
                (ParamMode::Inout, false)
            } else if self.eat(TokenKind::Borrow) {
                (ParamMode::Borrow, false)
            } else if self.eat(TokenKind::Mut) {
                (ParamMode::Normal, true)
            } else {
                (ParamMode::Normal, false)
            };
            if self.at(TokenKind::SelfValue) {
                let tok = self.bump();
                receiver = Some(SelfParam {
                    mode,
                    is_mut,
                    span: Span::with_file(
                        self.file_id,
                        self.tokens[checkpoint].span.start,
                        tok.span.end,
                    ),
                });
                if self.eat(TokenKind::Comma) && !self.at(TokenKind::RParen) {
                    loop {
                        params.push(self.param()?);
                        if !self.eat(TokenKind::Comma) {
                            break;
                        }
                        if self.at(TokenKind::RParen) {
                            break;
                        }
                    }
                }
            } else {
                self.cursor = checkpoint;
                loop {
                    params.push(self.param()?);
                    if !self.eat(TokenKind::Comma) {
                        break;
                    }
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        let return_type = if self.eat(TokenKind::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        let body = Expr::Block(self.block()?);
        Ok(Method {
            directives,
            name,
            receiver,
            params,
            return_type,
            body,
            span: self.span_from(start),
        })
    }

    fn anonymous_drop_method(&mut self) -> PResult<Method> {
        let start = self.start();
        self.expect(TokenKind::Drop)?;
        self.expect(TokenKind::Fn)?;
        self.expect(TokenKind::LParen)?;
        let tok = self.expect(TokenKind::SelfValue)?;
        self.expect(TokenKind::RParen)?;
        let body = Expr::Block(self.block()?);
        let span = self.span_from(start);
        Ok(Method {
            directives: Directives::new(),
            name: Ident {
                name: self.syms.drop_marker,
                span,
            },
            receiver: Some(SelfParam {
                mode: ParamMode::Normal,
                is_mut: false,
                span: tok.span,
            }),
            params: Vec::new(),
            return_type: None,
            body,
            span,
        })
    }

    fn array_length(&mut self) -> PResult<ArrayLength> {
        match self.kind() {
            TokenKind::Int(n) => {
                self.bump();
                Ok(ArrayLength::Literal(n))
            }
            TokenKind::Ident(_) => {
                let name = self.ident()?;
                if self.eat(TokenKind::LParen) {
                    let mut args = Vec::new();
                    if !self.at(TokenKind::RParen) {
                        loop {
                            args.push(self.array_length()?);
                            if !self.eat(TokenKind::Comma) {
                                break;
                            }
                            if self.at(TokenKind::RParen) {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen)?;
                    Ok(ArrayLength::Call { name, args })
                } else {
                    Ok(ArrayLength::Named(name))
                }
            }
            _ => {
                self.unexpected("array length");
                Err(())
            }
        }
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.pratt(0)
    }
    fn pratt(&mut self, min_bp: u8) -> PResult<Expr> {
        let lhs = match self.kind() {
            TokenKind::Minus | TokenKind::Bang | TokenKind::Tilde => {
                let token = self.bump();
                let op = match token.kind {
                    TokenKind::Minus => UnaryOp::Neg,
                    TokenKind::Bang => UnaryOp::Not,
                    _ => UnaryOp::BitNot,
                };
                let operand = self.pratt(19)?;
                let span = Span::with_file(self.file_id, token.span.start, operand.span().end);
                Expr::Unary(UnaryExpr {
                    op,
                    operand: Box::new(operand),
                    span,
                })
            }
            _ => self.primary()?,
        };
        self.pratt_tail(lhs, min_bp)
    }

    fn pratt_tail(&mut self, lhs: Expr, min_bp: u8) -> PResult<Expr> {
        let mut lhs = self.postfix(lhs)?;
        loop {
            let Some((lbp, rbp, op)) = binary_binding(self.kind()) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.bump();
            let rhs = self.pratt(rbp)?;
            let span = lhs.span().extend_to(rhs.span().end);
            lhs = Expr::Binary(BinaryExpr {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
                span,
            });
        }
        if matches!(self.kind(), TokenKind::Ident(name) if name == self.syms.as_kw) {
            self.error(
                "Rue has no 'as' cast operator; use '@intCast(value)' (the target type comes \
                 from context) or give the target type on the binding: 'let x: i64 = value'",
            );
            return Err(());
        }
        Ok(lhs)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.start();
        match self.kind() {
            TokenKind::Int(value) => {
                let span = self.bump().span;
                Ok(Expr::Int(IntLit { value, span }))
            }
            TokenKind::String(value) => {
                let span = self.bump().span;
                Ok(Expr::String(StringLit { value, span }))
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                let span = self.bump().span;
                Ok(Expr::Bool(BoolLit { value, span }))
            }
            TokenKind::LParen => {
                self.bump();
                if self.eat(TokenKind::RParen) {
                    Ok(Expr::Unit(UnitLit {
                        span: self.span_from(start),
                    }))
                } else {
                    let inner = Box::new(self.expr()?);
                    if !self.at(TokenKind::RParen) {
                        let expected = if self.at(TokenKind::Eq) {
                            "'(' or '{' or '.' or '[' or …"
                        } else {
                            "'.' or '[' or '*' or '/' or …"
                        };
                        self.unexpected(expected);
                        return Err(());
                    }
                    self.bump();
                    Ok(Expr::Paren(ParenExpr {
                        inner,
                        span: self.span_from(start),
                    }))
                }
            }
            TokenKind::LBrace => self.block().map(Expr::Block),
            TokenKind::LBracket => self.array_lit(),
            TokenKind::Ident(_) => self.ident_expr(),
            TokenKind::SelfValue => {
                let span = self.bump().span;
                Ok(Expr::SelfExpr(SelfExpr { span }))
            }
            TokenKind::SelfType => {
                let token = self.bump();
                let mut name = Ident {
                    name: self.syms.self_type,
                    span: token.span,
                };
                if self.at(TokenKind::LBrace) {
                    let fields = self.field_inits()?;
                    let span = self.span_from(start);
                    name.span = span;
                    Ok(Expr::StructLit(StructLitExpr {
                        base: None,
                        name,
                        ctor_args: None,
                        fields,
                        span,
                    }))
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            TokenKind::At => self.intrinsic(),
            TokenKind::If => self.if_expr(),
            TokenKind::While => self.while_expr(),
            TokenKind::Loop => self.loop_expr(),
            TokenKind::For => self.for_expr(),
            TokenKind::Match => self.match_expr(),
            TokenKind::Break => {
                self.bump();
                let value = if self.at(TokenKind::LBrace) || self.expr_terminator() {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                Ok(Expr::Break(BreakExpr {
                    value,
                    span: self.span_from(start),
                }))
            }
            TokenKind::Continue => {
                self.bump();
                Ok(Expr::Continue(ContinueExpr {
                    span: self.span_from(start),
                }))
            }
            TokenKind::Return => {
                self.bump();
                let value = if self.at(TokenKind::LBrace) || self.expr_terminator() {
                    None
                } else {
                    Some(Box::new(self.expr()?))
                };
                Ok(Expr::Return(ReturnExpr {
                    value,
                    span: self.span_from(start),
                }))
            }
            TokenKind::Comptime => {
                self.bump();
                let inner = Expr::Block(self.block()?);
                Ok(Expr::Comptime(ComptimeBlockExpr {
                    expr: Box::new(inner),
                    span: self.span_from(start),
                }))
            }
            TokenKind::Checked => {
                self.bump();
                let inner = Expr::Block(self.block()?);
                Ok(Expr::Checked(CheckedBlockExpr {
                    expr: Box::new(inner),
                    span: self.span_from(start),
                }))
            }
            TokenKind::Struct => {
                let type_expr = self.anonymous_struct_type(true)?;
                Ok(Expr::TypeLit(TypeLitExpr {
                    span: type_expr.span(),
                    type_expr,
                }))
            }
            TokenKind::Enum => {
                self.bump();
                let variants = self.enum_variants()?;
                let span = self.span_from(start);
                Ok(Expr::TypeLit(TypeLitExpr {
                    type_expr: TypeExpr::AnonymousEnum { variants, span },
                    span,
                }))
            }
            kind if self.primitive_spur(kind).is_some() => {
                let type_expr = self.ty()?;
                Ok(Expr::TypeLit(TypeLitExpr {
                    span: type_expr.span(),
                    type_expr,
                }))
            }
            _ => {
                self.unexpected("expression");
                Err(())
            }
        }
    }

    fn expr_terminator(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Semi
                | TokenKind::Comma
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::FatArrow
                | TokenKind::Eof
        )
    }

    fn ident_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        let name = self.ident()?;
        let name_end = name.span.end;
        if self.at(TokenKind::LParen) {
            let recovered_empty_args =
                self.nth(1) == TokenKind::Comma && self.nth(2) == TokenKind::RParen;
            let args = self.call_args()?;
            if recovered_empty_args {
                self.error_at(
                    "expected semicolon after expression",
                    Span::with_file(self.file_id, start, name_end),
                );
            }
            if self.at(TokenKind::LBrace) && self.looks_like_fields() {
                let fields = self.field_inits()?;
                Ok(Expr::StructLit(StructLitExpr {
                    base: None,
                    name,
                    ctor_args: Some(args),
                    fields,
                    span: self.span_from(start),
                }))
            } else {
                Ok(Expr::Call(CallExpr {
                    name,
                    args,
                    span: if recovered_empty_args {
                        Span::with_file(self.file_id, start, name_end)
                    } else {
                        self.span_from(start)
                    },
                }))
            }
        } else if self.at(TokenKind::LBrace) && self.looks_like_fields() {
            let fields = self.field_inits()?;
            Ok(Expr::StructLit(StructLitExpr {
                base: None,
                name,
                ctor_args: None,
                fields,
                span: self.span_from(start),
            }))
        } else {
            Ok(Expr::Ident(name))
        }
    }

    fn postfix(&mut self, mut base: Expr) -> PResult<Expr> {
        loop {
            match self.kind() {
                TokenKind::Dot => {
                    self.bump();
                    let field = self.ident()?;
                    if self.at(TokenKind::LParen) {
                        let args = self.call_args()?;
                        // A module-qualified inline type-constructor struct-literal
                        // head (`m.F(args) { ... }`, RUE-951) mirrors the local
                        // form in `ident_expr`: the `(...)` is the ctor call and a
                        // following field-shaped `{...}` is the struct literal, with
                        // the receiver carried as the `base`. Any other continuation
                        // is an ordinary method call. The struct-literal-in-a-bare-
                        // condition guard is post-hoc in `condition_body`/`match_expr`
                        // via `tail_is_struct_lit`, exactly as for the local form.
                        if self.at(TokenKind::LBrace) && self.looks_like_fields() {
                            let fields = self.field_inits()?;
                            let span = base.span().extend_to(self.previous_end());
                            base = Expr::StructLit(StructLitExpr {
                                base: Some(Box::new(base)),
                                name: field,
                                ctor_args: Some(args),
                                fields,
                                span,
                            });
                        } else {
                            let span = base.span().extend_to(self.previous_end());
                            base = Expr::MethodCall(MethodCallExpr {
                                receiver: Box::new(base),
                                method: field,
                                args,
                                span,
                            });
                        }
                    } else if self.at(TokenKind::LBrace) && self.looks_like_fields() {
                        let fields = self.field_inits()?;
                        let span = base.span().extend_to(self.previous_end());
                        base = Expr::StructLit(StructLitExpr {
                            base: Some(Box::new(base)),
                            name: field,
                            ctor_args: None,
                            fields,
                            span,
                        });
                    } else {
                        let span = base.span().extend_to(field.span.end);
                        base = Expr::Field(FieldExpr {
                            base: Box::new(base),
                            field,
                            span,
                        });
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.expr()?;
                    self.expect(TokenKind::RBracket)?;
                    let span = base.span().extend_to(self.previous_end());
                    base = Expr::Index(IndexExpr {
                        base: Box::new(base),
                        index: Box::new(index),
                        span,
                    });
                }
                TokenKind::Question => {
                    let end = self.bump().span.end;
                    let span = base.span().extend_to(end);
                    base = Expr::Try(TryExpr {
                        operand: Box::new(base),
                        span,
                    });
                }
                _ => break,
            }
        }
        Ok(base)
    }

    fn looks_like_fields(&self) -> bool {
        if !self.at(TokenKind::LBrace) {
            return false;
        }
        matches!(
            (self.nth(1), self.nth(2)),
            (TokenKind::RBrace, _)
                | (
                    TokenKind::Ident(_),
                    TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace
                )
        )
    }

    fn call_args(&mut self) -> PResult<Vec<CallArg>> {
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if self.at(TokenKind::Comma) && self.nth(1) == TokenKind::RParen {
            self.bump();
            self.bump();
            return Ok(args);
        }
        if !self.at(TokenKind::RParen) {
            loop {
                let start = self.start();
                let mode = if self.eat(TokenKind::Inout) {
                    ArgMode::Inout
                } else if self.eat(TokenKind::Borrow) {
                    ArgMode::Borrow
                } else {
                    ArgMode::Normal
                };
                let expr = self.expr()?;
                args.push(CallArg {
                    mode,
                    expr,
                    span: self.span_from(start),
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RParen) {
                    break;
                }
            }
        }
        if !self.at(TokenKind::RParen) && self.at(TokenKind::Eq) {
            self.unexpected("'(' or '{' or '.' or '[' or …");
            return Err(());
        }
        self.expect(TokenKind::RParen)?;
        Ok(args)
    }

    fn field_inits(&mut self) -> PResult<Vec<FieldInit>> {
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        if !self.at(TokenKind::RBrace) {
            loop {
                let start = self.start();
                let name = self.ident()?;
                let (value, shorthand) = if self.eat(TokenKind::Colon) {
                    (Box::new(self.expr()?), false)
                } else {
                    (Box::new(Expr::Ident(name)), true)
                };
                fields.push(FieldInit {
                    name,
                    value,
                    shorthand,
                    span: self.span_from(start),
                });
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RBrace) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(fields)
    }

    fn array_lit(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.expect(TokenKind::LBracket)?;
        let mut elements = Vec::new();
        let mut repeat = None;
        if !self.at(TokenKind::RBracket) {
            let first = self.expr()?;
            elements.push(first);
            if self.eat(TokenKind::Semi) {
                repeat = Some(match self.kind() {
                    TokenKind::Int(value) => {
                        self.bump();
                        ArrayLength::Literal(value)
                    }
                    TokenKind::Ident(_) => ArrayLength::Named(self.ident()?),
                    _ => {
                        self.error("expected literal or named array repeat count");
                        return Err(());
                    }
                });
            } else {
                while self.eat(TokenKind::Comma) {
                    if self.at(TokenKind::RBracket) {
                        break;
                    }
                    elements.push(self.expr()?);
                }
            }
        }
        self.expect(TokenKind::RBracket)?;
        Ok(Expr::ArrayLit(ArrayLitExpr {
            elements,
            repeat,
            span: self.span_from(start),
        }))
    }

    fn intrinsic(&mut self) -> PResult<Expr> {
        let start = self.start();
        let name = match self.kind() {
            TokenKind::At => {
                let at = self.bump();
                if self.at(TokenKind::Drop) {
                    let span = self.bump().span;
                    Ident {
                        name: self.syms.drop_kw,
                        span,
                    }
                } else {
                    match self.ident_expected("identifier or 'drop'") {
                        Ok(name) => name,
                        Err(()) => {
                            let previous_end = at.span.start.saturating_sub(5);
                            self.errors.push(CompileError::new(
                                ErrorKind::UnexpectedToken {
                                    expected: "identifier".into(),
                                    found: "'@'".into(),
                                },
                                Span::with_file(self.file_id, at.span.start, previous_end),
                            ));
                            return Err(());
                        }
                    }
                }
            }
            _ => unreachable!(),
        };
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if !self.at(TokenKind::RParen) {
            loop {
                let is_unambiguous_ty = self.primitive_spur(self.kind()).is_some()
                    || (self.at(TokenKind::LBracket) && self.bracket_is_array_type())
                    || (self.at(TokenKind::LParen) && self.nth(1) == TokenKind::RParen)
                    || (self.at(TokenKind::Bang)
                        && matches!(self.nth(1), TokenKind::Comma | TokenKind::RParen));
                if is_unambiguous_ty {
                    args.push(IntrinsicArg::Type(self.ty()?));
                } else {
                    args.push(IntrinsicArg::Expr(self.expr()?));
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RParen) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        if name.name == self.syms.allow_directive || name.name == self.syms.copy_directive {
            self.error_at("directive must precede a statement", self.span_from(start));
            return Err(());
        }
        Ok(Expr::IntrinsicCall(IntrinsicCallExpr {
            name,
            args,
            span: self.span_from(start),
        }))
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.expect(TokenKind::If)?;
        let cond = self.expr()?;
        let (cond, then_block) = self.condition_body(cond, "if", start)?;
        let else_block = if self.eat(TokenKind::Else) {
            if self.at(TokenKind::If) {
                let nested = self.if_expr()?;
                let span = nested.span();
                Some(BlockExpr {
                    statements: Vec::new(),
                    expr: Box::new(nested),
                    span,
                })
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Expr::If(IfExpr {
            cond: Box::new(cond),
            then_block,
            else_block,
            span: self.span_from(start),
        }))
    }
    fn while_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.bump();
        let cond = self.expr()?;
        let (cond, body) = self.condition_body(cond, "while", start)?;
        Ok(Expr::While(WhileExpr {
            cond: Box::new(cond),
            body,
            span: self.span_from(start),
        }))
    }
    fn for_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.bump();
        let binder = self.let_pattern(false)?;
        self.expect(TokenKind::In)?;
        let iterable = self.expr()?;
        let (iterable, body) = self.condition_body(iterable, "for", start)?;
        Ok(Expr::For(ForExpr {
            binder,
            iterable: Box::new(iterable),
            body,
            span: self.span_from(start),
        }))
    }
    fn loop_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.bump();
        let body = self.block()?;
        Ok(Expr::Loop(LoopExpr {
            body,
            span: self.span_from(start),
        }))
    }

    fn condition_body(
        &mut self,
        head: Expr,
        context: &str,
        control_start: u32,
    ) -> PResult<(Expr, BlockExpr)> {
        if self.at(TokenKind::LBrace) {
            if condition::tail_is_struct_lit(&head) {
                let noun = if context == "for" {
                    "iterable"
                } else {
                    "condition"
                };
                let body_end = self.skip_brace_group();
                let span = if context == "if" {
                    self.tokens
                        .get(self.cursor)
                        .map(|token| token.span)
                        .unwrap_or_else(|| Span::point_in_file(self.file_id, body_end))
                } else {
                    Span::with_file(self.file_id, control_start, body_end)
                };
                self.error_at(
                    format!("struct literals are not allowed as a bare {context} {noun}; wrap the {noun} in parentheses"),
                    span,
                );
                return Err(());
            }
            return Ok((head, self.block()?));
        }
        if let Some(pair) = condition::reclaim_as_condition_and_body(head) {
            Ok(pair)
        } else {
            self.error(format!(
                "expected '{{' and body block after the {context} condition"
            ));
            Err(())
        }
    }

    fn skip_brace_group(&mut self) -> u32 {
        debug_assert!(self.at(TokenKind::LBrace));
        let mut depth = 0usize;
        let mut end = self.start();
        while !self.at(TokenKind::Eof) {
            let token = self.bump();
            end = token.span.end;
            match token.kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        end
    }

    fn bracket_is_array_type(&self) -> bool {
        let mut depth = 0usize;
        for token in &self.tokens[self.cursor..] {
            match token.kind {
                TokenKind::LBracket | TokenKind::LParen | TokenKind::LBrace => depth += 1,
                TokenKind::RBracket => {
                    if depth == 1 {
                        return false;
                    }
                    depth = depth.saturating_sub(1);
                }
                TokenKind::RParen | TokenKind::RBrace => depth = depth.saturating_sub(1),
                TokenKind::Semi if depth == 1 => return true,
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.bump();
        let scrutinee = self.expr()?;
        let (scrutinee, empty_arms_consumed) = if self.at(TokenKind::LBrace) {
            if matches!(scrutinee, Expr::StructLit(_)) {
                let body_end = self.skip_brace_group();
                self.error_at(
                    "struct literals are not allowed as a bare match scrutinee; wrap the scrutinee in parentheses",
                    Span::with_file(self.file_id, start, body_end),
                );
                return Err(());
            }
            (scrutinee, false)
        } else if let Expr::StructLit(lit) = scrutinee {
            if lit.fields.is_empty() {
                (
                    condition::reclaim_empty_struct_lit_head(lit).ok_or_else(|| {
                        self.error("struct literals are not allowed as a bare match scrutinee");
                    })?,
                    true,
                )
            } else {
                self.error("struct literals are not allowed as a bare match scrutinee");
                return Err(());
            }
        } else {
            self.error("expected match arms");
            return Err(());
        };
        if empty_arms_consumed {
            return Ok(Expr::Match(MatchExpr {
                scrutinee: Box::new(scrutinee),
                arms: Vec::new(),
                span: self.span_from(start),
            }));
        }
        self.expect(TokenKind::LBrace)?;
        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let arm_start = self.start();
            let pattern = self.pattern()?;
            self.expect(TokenKind::FatArrow)?;
            let body = Box::new(self.expr()?);
            arms.push(MatchArm {
                pattern,
                body,
                span: self.span_from(arm_start),
            });
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(Expr::Match(MatchExpr {
            scrutinee: Box::new(scrutinee),
            arms,
            span: self.span_from(start),
        }))
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.start();
        match self.kind() {
            TokenKind::Underscore => {
                let span = self.bump().span;
                Ok(Pattern::Wildcard(span))
            }
            TokenKind::Int(value) => {
                let span = self.bump().span;
                Ok(Pattern::Int(IntLit { value, span }))
            }
            TokenKind::Minus => {
                self.bump();
                if let TokenKind::Int(value) = self.bump().kind {
                    Ok(Pattern::NegInt(NegIntLit {
                        value,
                        span: self.span_from(start),
                    }))
                } else {
                    self.error("expected integer after '-'");
                    Err(())
                }
            }
            TokenKind::True | TokenKind::False => {
                let value = self.at(TokenKind::True);
                let span = self.bump().span;
                Ok(Pattern::Bool(BoolLit { value, span }))
            }
            TokenKind::Ident(_) => self.path_pattern(start),
            _ => {
                self.error("expected pattern");
                Err(())
            }
        }
    }

    /// With the cursor at a `(`, scan to the matching `)` and report whether the
    /// token immediately after it is a `.`. This distinguishes an inline
    /// type-constructor call on a pattern head (`Result(i32, i32).Ok`, the group
    /// precedes a dot) from a variant's payload bindings (`Ok(v)`, the group is
    /// terminal) during a single left-to-right pass (RUE-947).
    fn paren_group_precedes_dot(&self) -> bool {
        debug_assert!(self.at(TokenKind::LParen));
        let mut cursor = self.cursor;
        let mut depth = 0usize;
        while let Some(token) = self.tokens.get(cursor) {
            match token.kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return self.tokens.get(cursor + 1).map(|t| t.kind) == Some(TokenKind::Dot);
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            cursor += 1;
        }
        false
    }

    fn path_pattern(&mut self, start: u32) -> PResult<Pattern> {
        let first = self.ident()?;
        // The inline type-constructor call (RUE-596) attaches to the `type_name`
        // segment — the last one before the variant. For a local head that is
        // the first ident (`Result(i32, i32).Ok(v)`); for a module-qualified
        // head it is a later segment (`std.result.Result(i32, i32).Ok(v)`,
        // RUE-947). A `(...)` group is the ctor call only when it precedes a
        // dot; a terminal `(...)` is the variant's payload bindings.
        let mut ctor_args = None;
        if self.at(TokenKind::LParen) {
            ctor_args = Some(self.call_args()?);
        }
        self.expect(TokenKind::Dot)?;
        let mut segments = vec![self.ident()?];
        if ctor_args.is_none() && self.at(TokenKind::LParen) && self.paren_group_precedes_dot() {
            ctor_args = Some(self.call_args()?);
        }
        while self.eat(TokenKind::Dot) {
            segments.push(self.ident()?);
            if ctor_args.is_none() && self.at(TokenKind::LParen) && self.paren_group_precedes_dot()
            {
                ctor_args = Some(self.call_args()?);
            }
        }
        let variant = segments.pop().unwrap();
        let (type_name, base) = if segments.is_empty() {
            (first, None)
        } else {
            let type_name = segments.pop().unwrap();
            let mut expr = Expr::Ident(first);
            for field in segments {
                let span = expr.span().extend_to(field.span.end);
                expr = Expr::Field(FieldExpr {
                    base: Box::new(expr),
                    field,
                    span,
                });
            }
            (type_name, Some(Box::new(expr)))
        };
        let mut bindings = Vec::new();
        if self.eat(TokenKind::LParen) {
            if self.at(TokenKind::RParen) {
                self.error("expected payload binding");
                return Err(());
            }
            loop {
                if self.at(TokenKind::Underscore) {
                    let span = self.bump().span;
                    bindings.push(Ident {
                        name: self.syms.underscore,
                        span,
                    });
                } else {
                    bindings.push(self.ident()?);
                }
                if !self.eat(TokenKind::Comma) {
                    break;
                }
                if self.at(TokenKind::RParen) {
                    break;
                }
            }
            self.expect(TokenKind::RParen)?;
        }
        Ok(Pattern::Path(PathPattern {
            base,
            type_name,
            ctor_args,
            variant,
            bindings,
            span: self.span_from(start),
        }))
    }

    fn let_pattern(&mut self, after_mut: bool) -> PResult<LetPattern> {
        if self.at(TokenKind::Underscore) {
            Ok(LetPattern::Wildcard(self.bump().span))
        } else {
            self.ident_expected(if after_mut {
                "identifier or '_'"
            } else {
                "'mut' or identifier or '_'"
            })
            .map(LetPattern::Ident)
        }
    }

    fn block(&mut self) -> PResult<BlockExpr> {
        let start = self.start();
        self.expect(TokenKind::LBrace)?;
        let mut statements = Vec::new();
        let mut final_expr = None;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let directed_let = self.at(TokenKind::At) && self.directive_is_followed_by_let();
            if self.at(TokenKind::Let) || (self.at(TokenKind::At) && directed_let) {
                statements.push(self.let_statement()?);
                continue;
            }
            // In statement position a block-like expression (`if`/`match`/
            // `while`/`loop`/`for`/`{ ... }`) forms a complete statement on its
            // own; it does not continue into an enclosing infix expression
            // (RUE-918). A following `-` starts a NEW statement (unary
            // negation), preserving the RUE-210 rule; any other infix binary
            // operator is a syntax error that points at parentheses to opt into
            // expression use. Postfix continuations (`.method()`, indexing,
            // calls) are still routed through Pratt here; that boundary is left
            // untouched pending the RUE-922 design work. Non-statement uses
            // (`let` right-hand sides, parenthesized forms) never reach this arm
            // and continue through Pratt normally.
            let expr_start = self.cursor;
            let value = if matches!(
                self.kind(),
                TokenKind::If
                    | TokenKind::Match
                    | TokenKind::While
                    | TokenKind::Loop
                    | TokenKind::For
                    | TokenKind::LBrace
            ) {
                let block_like = self.primary()?;
                if is_control_flow(&block_like) {
                    if self.at(TokenKind::Minus) {
                        statements.push(Statement::Expr(block_like));
                        continue;
                    }
                    if binary_binding(self.kind()).is_some() {
                        let op_span = self
                            .tokens
                            .get(self.cursor)
                            .map(|token| token.span)
                            .unwrap_or_else(|| {
                                Span::point_in_file(self.file_id, self.end_offset())
                            });
                        self.errors.push(
                            CompileError::new(
                                ErrorKind::ParseError(
                                    "a block-like expression in statement position is a \
                                     complete statement; a binary operator cannot continue it"
                                        .to_owned(),
                                ),
                                op_span,
                            )
                            .with_help(
                                "wrap the construct in parentheses to use it as a value, \
                                 e.g. `(if c { a } else { b }) + x`",
                            ),
                        );
                        return Err(());
                    }
                }
                self.pratt_tail(block_like, 0)?
            } else {
                self.expr()?
            };
            if self.eat(TokenKind::Eq) {
                let target = expr_to_target(value, self.syms.self_value).ok_or_else(|| {
                    self.error("invalid assignment target");
                })?;
                let rhs = Box::new(self.expr()?);
                self.expect(TokenKind::Semi)?;
                statements.push(Statement::Assign(AssignStatement {
                    target,
                    value: rhs,
                    span: Span::with_file(
                        self.file_id,
                        self.tokens[expr_start].span.start,
                        self.previous_end(),
                    ),
                }));
                continue;
            }
            if self.eat(TokenKind::Semi) {
                statements.push(Statement::Expr(value));
                continue;
            }
            if self.at(TokenKind::RBrace) {
                final_expr = Some(value);
                break;
            }
            if is_control_flow(&value) {
                statements.push(Statement::Expr(value));
                continue;
            }
            self.error_at("expected semicolon after expression", value.span());
            return Err(());
        }
        self.expect(TokenKind::RBrace)?;
        let span = self.span_from(start);
        let expr = final_expr.unwrap_or_else(|| {
            if matches!(statements.last(), Some(Statement::Expr(e)) if is_diverging(e)) {
                if let Some(Statement::Expr(e)) = statements.pop() {
                    return e;
                }
            }
            Expr::Unit(UnitLit {
                span: Span::point_in_file(self.file_id, span.end),
            })
        });
        Ok(BlockExpr {
            statements,
            expr: Box::new(expr),
            span,
        })
    }

    fn let_statement(&mut self) -> PResult<Statement> {
        let start = self.start();
        let directives = self.directives()?;
        self.expect(TokenKind::Let)?;
        let is_mut = self.eat(TokenKind::Mut);
        let pattern = self.let_pattern(is_mut)?;
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.ty()?)
        } else {
            None
        };
        if !self.at(TokenKind::Eq) {
            self.unexpected("':' or '='");
            return Err(());
        }
        self.bump();
        let init = Box::new(self.expr()?);
        if !self.at(TokenKind::Semi) {
            self.unexpected("'.' or '[' or '*' or '/' or …");
            return Err(());
        }
        self.bump();
        Ok(Statement::Let(LetStatement {
            directives,
            is_mut,
            pattern,
            ty,
            init,
            span: self.span_from(start),
        }))
    }

    fn directive_is_followed_by_let(&self) -> bool {
        let mut cursor = self.cursor;
        let mut directive_count = 0usize;
        while self.tokens.get(cursor).map(|token| token.kind) == Some(TokenKind::At) {
            directive_count += 1;
            cursor += 1;
            if !matches!(
                self.tokens.get(cursor).map(|token| token.kind),
                Some(TokenKind::Ident(_))
            ) {
                return false;
            }
            cursor += 1;
            if self.tokens.get(cursor).map(|token| token.kind) == Some(TokenKind::LParen) {
                let mut depth = 1usize;
                cursor += 1;
                while let Some(token) = self.tokens.get(cursor) {
                    match token.kind {
                        TokenKind::LParen => depth += 1,
                        TokenKind::RParen => {
                            depth -= 1;
                            if depth == 0 {
                                cursor += 1;
                                break;
                            }
                        }
                        TokenKind::Eof => return false,
                        _ => {}
                    }
                    cursor += 1;
                }
                if depth != 0 {
                    return false;
                }
            }
        }
        directive_count != 0
            && self.tokens.get(cursor).map(|token| token.kind) == Some(TokenKind::Let)
    }

    fn recover_item(&mut self) -> Span {
        let start = self
            .tokens
            .get(self.cursor)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
        if self.recover_reserved_let_keywords() || self.recover_missing_item_name() {
            return start;
        }
        if !self.at(TokenKind::Eof) {
            debug_assert_eq!(
                recovery::item_recovery_action(
                    recovery::ItemRecoveryPosition::Initial,
                    &self.kind()
                ),
                recovery::ItemRecoveryAction::Consume
            );
            self.bump();
        }
        while !self.at(TokenKind::Eof)
            && recovery::item_recovery_action(
                recovery::ItemRecoveryPosition::AfterProgress,
                &self.kind(),
            ) == recovery::ItemRecoveryAction::Consume
        {
            self.bump();
        }
        start
    }

    fn recover_reserved_let_keywords(&mut self) -> bool {
        let Some(initial_span) = self.errors.last().and_then(CompileError::span) else {
            return false;
        };
        let is_binding_error = matches!(
            self.errors.last().map(|error| &error.kind),
            Some(ErrorKind::UnexpectedToken { expected, .. })
                if expected == "'mut' or identifier or '_'"
        );
        if !is_binding_error {
            return false;
        }

        let mut brace_depth = 0usize;
        while !self.at(TokenKind::Eof) {
            let token = self.tokens[self.cursor].clone();
            if brace_depth > 0 && token.span.start >= initial_span.start {
                let expected = match token.kind {
                    TokenKind::Fn | TokenKind::Struct | TokenKind::Const => Some("identifier"),
                    TokenKind::Unchecked => Some("'fn'"),
                    TokenKind::Pub => Some("'unchecked' or 'fn' or 'linear' or 'struct' or …"),
                    _ => None,
                };
                if let Some(expected) = expected {
                    self.errors.push(CompileError::new(
                        ErrorKind::UnexpectedToken {
                            expected: expected.into(),
                            found: token.kind.name().to_owned().into(),
                        },
                        Span::with_file(
                            self.file_id,
                            token.span.start,
                            token.span.start.saturating_sub(1),
                        ),
                    ));
                }
            }
            match token.kind {
                TokenKind::LBrace => brace_depth += 1,
                TokenKind::RBrace => {
                    brace_depth = brace_depth.saturating_sub(1);
                    self.bump();
                    if brace_depth == 0 {
                        break;
                    }
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
        true
    }

    fn recover_missing_item_name(&mut self) -> bool {
        let missing_name = matches!(
            self.errors.last().map(|error| (&error.kind, error.span())),
            Some((ErrorKind::UnexpectedToken { expected, found }, Some(_)))
                if expected == "identifier" && found == "'{'"
        );
        if !missing_name || !matches!(self.kind(), TokenKind::Struct | TokenKind::Enum) {
            return false;
        }

        let mut saw_malformed_function = false;
        while !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Fn) {
                if matches!(self.nth(1), TokenKind::Ident(_)) {
                    break;
                }
                let token = self.bump();
                let previous_end = self
                    .cursor
                    .checked_sub(2)
                    .and_then(|index| self.tokens.get(index))
                    .map(|token| token.span.end)
                    .unwrap_or(token.span.start);
                self.errors.push(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "identifier".into(),
                        found: "'fn'".into(),
                    },
                    Span::with_file(self.file_id, token.span.start, previous_end),
                ));
                saw_malformed_function = true;
                continue;
            }
            if saw_malformed_function && self.at(TokenKind::Let) {
                let pattern_index = if self.nth(1) == TokenKind::Mut {
                    self.cursor + 2
                } else {
                    self.cursor + 1
                };
                if let Some(token) = self.tokens.get(pattern_index)
                    && !matches!(token.kind, TokenKind::Ident(_) | TokenKind::Underscore)
                {
                    self.errors.push(CompileError::new(
                        ErrorKind::UnexpectedToken {
                            expected: "identifier or '_'".into(),
                            found: token.kind.name().to_owned().into(),
                        },
                        token.span,
                    ));
                }
            }
            self.bump();
        }
        true
    }
}

fn binary_binding(kind: TokenKind) -> Option<(u8, u8, BinaryOp)> {
    let (p, op) = match kind {
        TokenKind::PipePipe => (1, BinaryOp::Or),
        TokenKind::AmpAmp => (3, BinaryOp::And),
        TokenKind::EqEq => (5, BinaryOp::Eq),
        TokenKind::BangEq => (5, BinaryOp::Ne),
        TokenKind::Lt => (5, BinaryOp::Lt),
        TokenKind::Gt => (5, BinaryOp::Gt),
        TokenKind::LtEq => (5, BinaryOp::Le),
        TokenKind::GtEq => (5, BinaryOp::Ge),
        TokenKind::Pipe => (7, BinaryOp::BitOr),
        TokenKind::Caret => (9, BinaryOp::BitXor),
        TokenKind::Amp => (11, BinaryOp::BitAnd),
        TokenKind::LtLt => (13, BinaryOp::Shl),
        TokenKind::GtGt => (13, BinaryOp::Shr),
        TokenKind::Plus => (15, BinaryOp::Add),
        TokenKind::Minus => (15, BinaryOp::Sub),
        TokenKind::Star => (17, BinaryOp::Mul),
        TokenKind::Slash => (17, BinaryOp::Div),
        TokenKind::Percent => (17, BinaryOp::Mod),
        _ => return None,
    };
    Some((p, p + 1, op))
}

fn expr_to_target(expr: Expr, self_value: Spur) -> Option<AssignTarget> {
    match expr {
        Expr::Ident(id) => Some(AssignTarget::Var(id)),
        Expr::Field(field) => Some(AssignTarget::Field(field)),
        Expr::Index(index) => Some(AssignTarget::Index(index)),
        // `self = value` targets the receiver binding; sema enforces that
        // only a `mut self` (or shadowing `let mut`) receiver is assignable.
        Expr::SelfExpr(se) => Some(AssignTarget::Var(Ident {
            name: self_value,
            span: se.span,
        })),
        _ => None,
    }
}
fn is_control_flow(expr: &Expr) -> bool {
    matches!(
        expr,
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
fn is_diverging(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Break(_) | Expr::Continue(_) | Expr::Return(_) | Expr::Loop(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_error::MAX_NESTING_DEPTH;
    use rue_lexer::Lexer;
    use std::time::{Duration, Instant};

    fn parse_source(source: &str) -> Result<(Ast, ThreadedRodeo), CompileErrors> {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner).parse()
    }

    #[test]
    fn representative_language_surface_parses() {
        for source in [
            "fn main(a: i32, borrow xs: [i32]) -> i32 { let mut x: i32 = a + 2 * 3; x = x - 1; if x > 0 { x } else { 0 } }",
            "@copy pub struct Pair { first: i32, second: i32, fn sum(borrow self) -> i32 { self.first + self.second } } enum Choice { None, One(i32), Pair(i32, i32), }",
            "fn use_all() -> i32 { let p = Pair { first: 1, second: 2 }; let a = [1, 2, 3,]; let b = [0; 4]; p.sum() + a[0] + b[1] }",
            "fn fixed_string(value: Str(8)) -> Str(8) { value }",
            "fn control(x: i32) -> i32 { while x > 10 { break; } match x { 0 => 1, -1 => 2, Choice.One(v) => v, _ => 0, } }",
            "fn boundaries() -> i32 { { 1 } -2 } fn qualified(x: pkg.Choice) -> i32 { match x { pkg.Choice.One(v) => v, _ => 0 } }",
            "fn generic(comptime T: type, comptime N: i32) -> type { struct { value: T, data: [i32; N], fn get(borrow self) -> T { self.value } } }",
            "pub const io = @import(\"io.rue\"); drop fn Pair(self) { @drop(self.first); }",
        ] {
            parse_source(source).unwrap_or_else(|errors| panic!("{errors:?}\n{source}"));
        }
    }

    #[test]
    fn multiple_directives_before_let_parse_as_one_statement() {
        let (ast, _) = parse_source(
            "fn main() -> i32 { \
             @allow(unused_variable, unreachable_code) \
             @allow(unused_function) \
             let x = 1; x }",
        )
        .unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected function");
        };
        let Expr::Block(body) = &function.body else {
            panic!("expected block body");
        };
        let Statement::Let(statement) = &body.statements[0] else {
            panic!("expected directed let statement");
        };
        assert_eq!(statement.directives.len(), 2);
        assert_eq!(statement.directives[0].args.len(), 2);
        assert_eq!(statement.directives[1].args.len(), 1);
    }

    #[test]
    fn reviewed_grammar_edges_are_bounded_and_targeted() {
        parse_source("fn f() { @probe([i32]); }").unwrap();
        parse_source("fn f() -> type { struct { fn make() -> i32 { 1 } } }").unwrap();
        for source in [
            "fn f() { let x = [0; count(1)]; }",
            "struct S { fn f() {} late: i32 }",
            "fn outer() -> type { struct { fn f() {} late: i32 } }",
            "fn outer() -> type { struct { a: i32 b: i32 } }",
            "fn f(x: struct { fn make() {} }) {}",
        ] {
            assert!(parse_source(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn rich_diagnostic_contract_is_preserved() {
        let errors = parse_source("fn main() -> i32 { let impl = 2; impl }").unwrap_err();
        let error = errors.first().unwrap();
        assert_eq!(
            error.kind,
            ErrorKind::UnexpectedToken {
                expected: "'mut' or identifier or '_'".into(),
                found: "'impl'".into(),
            }
        );
        assert_eq!(error.span(), Some(Span::new(23, 27)));
        assert_eq!(error.diagnostic().helps.len(), 1);

        let errors = parse_source("fn foo(a: [i32; ]) -> i32 { 0 }").unwrap_err();
        assert_eq!(
            errors.first().unwrap().kind,
            ErrorKind::UnexpectedToken {
                expected: "array length".into(),
                found: "']'".into(),
            }
        );
    }

    #[test]
    fn item_recovery_reports_each_malformed_function() {
        let errors =
            parse_source("fn foo( -> i32 { 0 }\nfn bar( -> i32 { 0 }\nfn baz( -> i32 { 0 }")
                .unwrap_err();
        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors
                .iter()
                .filter_map(|error| error.span().map(|span| span.start))
                .collect::<Vec<_>>(),
            vec![8, 29, 50]
        );
    }

    #[test]
    fn nesting_limit_is_checked_before_recursive_descent() {
        let depth = MAX_NESTING_DEPTH + 1;
        let source = format!(
            "fn main() -> i32 {{ {}0{} }}",
            "(".repeat(depth),
            ")".repeat(depth)
        );
        let errors = parse_source(&source).unwrap_err();
        assert!(matches!(
            errors.first().map(|error| &error.kind),
            Some(ErrorKind::NestingLimitExceeded { .. })
        ));
    }

    fn median_parse(source: &str, iterations: usize) -> Duration {
        let mut samples = Vec::new();
        for _ in 0..7 {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
                let start = Instant::now();
                Parser::new(tokens, interner).parse().unwrap();
                elapsed += start.elapsed();
            }
            samples.push(elapsed);
        }
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    #[test]
    fn nested_slice_parsing_avoids_exponential_growth() {
        let source = |depth: usize| {
            format!(
                "fn f(x: {}i32{}) {{}}",
                "[".repeat(depth),
                "]".repeat(depth)
            )
        };
        let shallow = median_parse(&source(32), 100);
        let deep = median_parse(&source(128), 100);
        assert!(
            // The full suite runs test binaries concurrently, so leave enough
            // scheduler headroom while still rejecting the former exponential
            // behavior (96 additional levels was effectively unbounded).
            deep < shallow.saturating_mul(32),
            "32 levels took {shallow:?}; 128 levels took {deep:?}"
        );
    }

    #[test]
    fn continued_blocks_avoid_exponential_speculative_reparsing() {
        let source = |count: usize| {
            format!(
                "fn f() -> i32 {{ {}0 }}",
                "if true { 1 } else { 2 }; ".repeat(count)
            )
        };
        let shallow = median_parse(&source(16), 50);
        let deep = median_parse(&source(64), 50);
        assert!(
            // A 4x input increase may be scheduled unevenly under the parallel
            // unit suite; the former 2^depth behavior exceeds this by orders
            // of magnitude.
            deep < shallow.saturating_mul(32),
            "16 blocks took {shallow:?}; 64 blocks took {deep:?}"
        );
    }
}
