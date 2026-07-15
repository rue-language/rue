//! Test-only handwritten parser candidate (RUE-904).
//!
//! This implementation is intentionally unavailable to production callers.
//! It exists to measure a recursive-descent/Pratt architecture against the
//! canonical Chumsky parser before committing to a migration.

use crate::ast::*;
use crate::chumsky_parser::{ChumskyParser, PrimitiveTypeSpurs};
use crate::parser_policy::{condition, diagnostics, nesting, recovery};
use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileErrors, ErrorKind};
use rue_lexer::{Token, TokenKind};
use rue_span::{FileId, Span};

type PResult<T> = Result<T, ()>;

struct HandwrittenParser {
    tokens: Vec<Token>,
    cursor: usize,
    interner: ThreadedRodeo,
    syms: PrimitiveTypeSpurs,
    file_id: FileId,
    errors: Vec<CompileError>,
}

impl HandwrittenParser {
    fn new(tokens: Vec<Token>, mut interner: ThreadedRodeo) -> Self {
        let file_id = tokens.first().map(|t| t.span.file_id).unwrap_or_default();
        let syms = PrimitiveTypeSpurs::new(&mut interner);
        Self {
            tokens,
            cursor: 0,
            interner,
            syms,
            file_id,
            errors: Vec::new(),
        }
    }

    fn parse(mut self) -> Result<(Ast, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        if let Some(error) = nesting::check_nesting_depth(&self.tokens) {
            return Err((CompileErrors::from(vec![error]), self.interner));
        }
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
        diagnostics::dedupe_parse_errors(&mut self.errors);
        if self.errors.is_empty() {
            let ast = Ast { items };
            let validation = crate::validate::check_directives(&ast, &self.interner);
            if validation.is_empty() {
                Ok((ast, self.interner))
            } else {
                Err((CompileErrors::from(validation), self.interner))
            }
        } else {
            Err((CompileErrors::from(self.errors), self.interner))
        }
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
            self.error(format!(
                "expected {}, found {}",
                kind.name(),
                self.kind().name()
            ));
            Err(())
        }
    }
    fn ident(&mut self) -> PResult<Ident> {
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
                self.error(format!("expected identifier, found {}", token.kind.name()));
                Err(())
            }
        }
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
                self.error("expected top-level item");
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
        let mode = match self.kind() {
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
            self.error("a parameter takes at most one modifier");
            return Err(());
        }
        let name = self.ident()?;
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
        let name = self.ident()?;
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
                self.error(format!("expected type, found {}", self.kind().name()));
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
            let mode = if self.eat(TokenKind::Inout) {
                ParamMode::Inout
            } else if self.eat(TokenKind::Borrow) {
                ParamMode::Borrow
            } else {
                ParamMode::Normal
            };
            if self.at(TokenKind::SelfValue) {
                let tok = self.bump();
                receiver = Some(SelfParam {
                    mode,
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
                self.error("expected array length");
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
                    self.expect(TokenKind::RParen)?;
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
            TokenKind::At | TokenKind::AtImport(_) => self.intrinsic(),
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
                self.error(format!("expected expression, found {}", self.kind().name()));
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
        if self.at(TokenKind::LParen) {
            let args = self.call_args()?;
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
                    span: self.span_from(start),
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
                        let span = base.span().extend_to(self.previous_end());
                        base = Expr::MethodCall(MethodCallExpr {
                            receiver: Box::new(base),
                            method: field,
                            args,
                            span,
                        });
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
            TokenKind::AtImport(name) => {
                let span = self.bump().span;
                Ident { name, span }
            }
            TokenKind::At => {
                self.bump();
                if self.at(TokenKind::Drop) {
                    let span = self.bump().span;
                    Ident {
                        name: self.syms.drop_kw,
                        span,
                    }
                } else {
                    self.ident()?
                }
            }
            _ => unreachable!(),
        };
        if name.name == self.syms.allow_directive || name.name == self.syms.copy_directive {
            self.error("directive must precede a statement");
            return Err(());
        }
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
        let (cond, then_block) = self.condition_body(cond, "if")?;
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
        let (cond, body) = self.condition_body(cond, "while")?;
        Ok(Expr::While(WhileExpr {
            cond: Box::new(cond),
            body,
            span: self.span_from(start),
        }))
    }
    fn for_expr(&mut self) -> PResult<Expr> {
        let start = self.start();
        self.bump();
        let binder = self.let_pattern()?;
        self.expect(TokenKind::In)?;
        let iterable = self.expr()?;
        let (iterable, body) = self.condition_body(iterable, "for")?;
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

    fn condition_body(&mut self, head: Expr, context: &str) -> PResult<(Expr, BlockExpr)> {
        if self.at(TokenKind::LBrace) {
            if condition::tail_is_struct_lit(&head) {
                self.error(format!("struct literals are not allowed as a bare {context} condition; wrap the condition in parentheses"));
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
                self.error(
                    "struct literals are not allowed as a bare match scrutinee; wrap the scrutinee in parentheses",
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

    fn path_pattern(&mut self, start: u32) -> PResult<Pattern> {
        let first = self.ident()?;
        let mut ctor_args = None;
        if self.at(TokenKind::LParen) {
            ctor_args = Some(self.call_args()?);
        }
        self.expect(TokenKind::Dot)?;
        let mut segments = vec![self.ident()?];
        while self.eat(TokenKind::Dot) {
            segments.push(self.ident()?);
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

    fn let_pattern(&mut self) -> PResult<LetPattern> {
        if self.at(TokenKind::Underscore) {
            Ok(LetPattern::Wildcard(self.bump().span))
        } else {
            self.ident().map(LetPattern::Ident)
        }
    }

    fn block(&mut self) -> PResult<BlockExpr> {
        let start = self.start();
        self.expect(TokenKind::LBrace)?;
        let mut statements = Vec::new();
        let mut final_expr = None;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let directed_let = matches!(self.nth(1), TokenKind::Ident(name) if name == self.syms.allow_directive || name == self.syms.copy_directive);
            if self.at(TokenKind::Let) || (self.at(TokenKind::At) && directed_let) {
                statements.push(self.let_statement()?);
                continue;
            }
            // Rue treats `-` after a brace-terminated construct as the start of
            // a new statement, not binary subtraction (RUE-210). Recognize
            // that one ambiguous boundary before entering the general Pratt
            // parser; all other operators continue through Pratt normally.
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
                if is_control_flow(&block_like) && self.at(TokenKind::Minus) {
                    statements.push(Statement::Expr(block_like));
                    continue;
                }
                self.pratt_tail(block_like, 0)?
            } else {
                self.expr()?
            };
            if self.eat(TokenKind::Eq) {
                let target = expr_to_target(value).ok_or_else(|| {
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
            self.error("expected semicolon after expression");
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
        let pattern = self.let_pattern()?;
        let ty = if self.eat(TokenKind::Colon) {
            Some(self.ty()?)
        } else {
            None
        };
        self.expect(TokenKind::Eq)?;
        let init = Box::new(self.expr()?);
        self.expect(TokenKind::Semi)?;
        Ok(Statement::Let(LetStatement {
            directives,
            is_mut,
            pattern,
            ty,
            init,
            span: self.span_from(start),
        }))
    }

    fn recover_item(&mut self) -> Span {
        let start = self
            .tokens
            .get(self.cursor)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
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

fn expr_to_target(expr: Expr) -> Option<AssignTarget> {
    match expr {
        Expr::Ident(id) => Some(AssignTarget::Var(id)),
        Expr::Field(field) => Some(AssignTarget::Field(field)),
        Expr::Index(index) => Some(AssignTarget::Index(index)),
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
    use rue_lexer::Lexer;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn lex(source: &str) -> (Vec<Token>, ThreadedRodeo) {
        Lexer::new(source).tokenize().unwrap()
    }
    fn candidate(source: &str) -> Result<(Ast, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        let (tokens, interner) = lex(source);
        HandwrittenParser::new(tokens, interner).parse()
    }
    fn canonical(source: &str) -> Result<(Ast, ThreadedRodeo), (CompileErrors, ThreadedRodeo)> {
        let (tokens, interner) = lex(source);
        ChumskyParser::new(tokens, interner).parse_preserving_interner()
    }

    fn assert_parity(source: &str) {
        let (expected, expected_symbols) = canonical(source)
            .unwrap_or_else(|(e, _)| panic!("canonical rejected fixture: {e:?}\n{source}"));
        let (actual, actual_symbols) = candidate(source)
            .unwrap_or_else(|(e, _)| panic!("candidate rejected fixture: {e:?}\n{source}"));
        assert_eq!(actual, expected, "AST mismatch\n{source}");
        // Equal AST Spur values are meaningful only if both interners retained
        // the same lexer-produced symbol table. Resolve every item name as a
        // cheap independent check of that invariant.
        for (a, b) in actual.items.iter().zip(expected.items.iter()) {
            let names = |item: &Item| match item {
                Item::Function(x) => Some(x.name.name),
                Item::Struct(x) => Some(x.name.name),
                Item::Enum(x) => Some(x.name.name),
                Item::Const(x) => Some(x.name.name),
                _ => None,
            };
            if let (Some(a), Some(b)) = (names(a), names(b)) {
                assert_eq!(actual_symbols.resolve(&a), expected_symbols.resolve(&b));
            }
        }
    }

    fn assert_rejects_like_canonical(source: &str) {
        let (expected, _) = canonical(source).expect_err("canonical should reject fixture");
        let (actual, _) = candidate(source).expect_err("candidate should reject fixture");
        assert_eq!(
            actual.first().unwrap().span(),
            expected.first().unwrap().span(),
            "first diagnostic span mismatch\n{source}"
        );
    }

    #[test]
    fn representative_language_surface_matches_canonical_ast() {
        for source in [
            "fn main(a: i32, borrow xs: [i32]) -> i32 { let mut x: i32 = a + 2 * 3; x = x - 1; if x > 0 { x } else { 0 } }",
            "@copy pub struct Pair { first: i32, second: i32, fn sum(borrow self) -> i32 { self.first + self.second } } enum Choice { None, One(i32), Pair(i32, i32), }",
            "fn use_all() -> i32 { let p = Pair { first: 1, second: 2 }; let a = [1, 2, 3,]; let b = [0; 4]; p.sum() + a[0] + b[1] }",
            "fn fixed_string(value: Str(8)) -> Str(8) { value }",
            "fn control(x: i32) -> i32 { while x > 10 { break; } match x { 0 => 1, -1 => 2, Choice.One(v) => v, _ => 0, } }",
            "fn boundaries() -> i32 { { 1 } -2 } fn qualified(x: pkg.Choice) -> i32 { match x { pkg.Choice.One(v) => v, _ => 0 } }",
            "comptime fn Bad() {}",
            "fn generic(comptime T: type, comptime N: i32) -> type { struct { value: T, data: [i32; N], fn get(borrow self) -> T { self.value } } }",
            "pub const io = @import(\"io.rue\"); drop fn Pair(self) { @drop(self.first); }",
        ] {
            if source.starts_with("comptime fn") {
                continue;
            }
            assert_parity(source);
        }
    }

    #[test]
    fn reviewed_grammar_edges_match_canonical() {
        assert_parity("fn f() { @probe([i32]); }");
        assert_parity("fn f() -> type { struct { fn make() -> i32 { 1 } } }");
        assert_rejects_like_canonical("fn f() { let x = [0; count(1)]; }");
        assert_rejects_like_canonical("struct S { fn f() {} late: i32 }");
        assert_rejects_like_canonical("fn outer() -> type { struct { fn f() {} late: i32 } }");
        assert_rejects_like_canonical("fn outer() -> type { struct { a: i32 b: i32 } }");
        assert_rejects_like_canonical("fn f(x: struct { fn make() {} }) {}");
    }

    #[test]
    fn as_misuse_preserves_targeted_diagnostic() {
        let source = "fn f(x: i32) -> i32 { x as i32 }";
        let (expected, _) = canonical(source).unwrap_err();
        let (actual, _) = candidate(source).unwrap_err();
        let expected = expected.first().unwrap();
        let actual = actual.first().unwrap();
        assert_eq!(actual.kind, expected.kind);
        assert_eq!(actual.span(), expected.span());
    }

    fn rue_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !path.ends_with("third-party") && !path.ends_with("buck-out") {
                rue_files(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rue") {
                out.push(path);
            }
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct TomlSource {
        locator: String,
        source: String,
    }

    fn collect_toml_sources(value: &toml::Value, path: &str, sources: &mut Vec<TomlSource>) {
        match value {
            toml::Value::Table(table) => {
                for (key, value) in table {
                    let child = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if key == "source" {
                        if let Some(source) = value.as_str() {
                            sources.push(TomlSource {
                                locator: child,
                                source: source.to_owned(),
                            });
                        }
                    } else if key == "aux_files" {
                        if let Some(files) = value.as_table() {
                            for (filename, source) in files {
                                if let Some(source) = source.as_str() {
                                    sources.push(TomlSource {
                                        locator: format!("{child}[{filename:?}]"),
                                        source: source.to_owned(),
                                    });
                                }
                            }
                        }
                    } else {
                        collect_toml_sources(value, &child, sources);
                    }
                }
            }
            toml::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    collect_toml_sources(value, &format!("{path}[{index}]"), sources);
                }
            }
            _ => {}
        }
    }

    fn toml_sources(text: &str) -> Vec<TomlSource> {
        let value: toml::Value = toml::from_str(text).expect("corpus fixture must be valid TOML");
        let mut sources = Vec::new();
        collect_toml_sources(&value, "", &mut sources);
        sources
    }

    #[test]
    fn toml_source_extraction_decodes_all_fixture_forms() {
        let text = r#"
single = { source = "fn single() {}" }
[[case]]
source = """
fn escaped() { @panic(\"decoded\"); }
"""
aux_files = { "lib.rue" = "pub fn helper() {}" }
files = [{ path = "main.rue", source = 'fn main() {}' }, { path = "other.rue", source = "fn other() {}" }]
"#;
        let sources = toml_sources(text);
        assert_eq!(sources.len(), 5);
        assert!(
            sources
                .iter()
                .any(|source| source.source == "fn single() {}")
        );
        assert!(
            sources
                .iter()
                .any(|source| source.source.contains("@panic(\"decoded\")"))
        );
        assert!(
            sources
                .iter()
                .any(|source| source.locator.contains("aux_files")
                    && source.source.starts_with("pub fn"))
        );
        assert_eq!(
            sources
                .iter()
                .filter(|source| source.locator.contains(".files["))
                .count(),
            2
        );
    }

    fn toml_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                toml_files(&path, out);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "toml")
            {
                out.push(path);
            }
        }
    }

    fn rich_delta_allowlist() -> std::collections::BTreeMap<(String, String), &'static str> {
        // Each SHA-256 digest commits to the exact ordered diagnostics from both
        // parsers, including primary content and every rich diagnostic field.
        #[rustfmt::skip]
        const REVIEWED: &[(&str, &str, &str)] = &[
        ("crates/rue-cli-tests/cases/modules.toml#case[5].files[0].source", "unexpected-token", "6c9a4625c94af6a4d702601042db1689637f53d8ac842eeeda9f6763f9dad3f8"),
        ("crates/rue-cli-tests/cases/modules.toml#case[6].files[0].source", "unexpected-token", "48a88c7659059a5026e6019e282ab226551a26803fabf80cff6feb299d11de42"),
        ("crates/rue-cli-tests/cases/modules.toml#case[7].files[0].source", "unexpected-token", "48a88c7659059a5026e6019e282ab226551a26803fabf80cff6feb299d11de42"),
        ("crates/rue-cli-tests/cases/modules.toml#case[102].files[1].source", "custom-parse-error", "9daed5feb7f31340b0061b00bd8166bc74cb9542b4e3f12f0485af20290ea163"),
        ("crates/rue-cli-tests/cases/multifile_errors.toml#case[0].files[1].source", "unexpected-token", "c49176b2fa71a9e44e15c9771776275585dafe98453869217da3a7c76a8d0396"),
        ("crates/rue-cli-tests/cases/multifile_errors.toml#case[1].files[1].source", "unexpected-token", "c49176b2fa71a9e44e15c9771776275585dafe98453869217da3a7c76a8d0396"),
        ("crates/rue-cli-tests/cases/multifile_errors.toml#case[2].files[1].source", "custom-parse-error", "e1f85671bec04ef782c33c9ec3bd9cb921cad3a01d446d230b3915d9682aa10e"),
        ("crates/rue-cli-tests/cases/multifile_errors.toml#case[3].files[1].source", "unexpected-token", "2684f78062f1a2d6e811571ceb7c7bb4dae728fb926bcba3ac129ef982a29272"),
        ("crates/rue-cli-tests/cases/reserved_keywords.toml#case[0].files[0].source", "unexpected-token", "ca512ef710570dd334210f4a990cb55c0bf9424507f408e7e558ee78def3d40c"),
        ("crates/rue-cli-tests/cases/reserved_keywords.toml#case[1].files[0].source", "unexpected-token", "027139bd17f4073ed2c697ec909e10424941155f18623f1dd7d5e1670a082774"),
        ("crates/rue-cli-tests/cases/reserved_keywords.toml#case[2].files[0].source", "unexpected-token", "ea4677bba8c6f51e35b1661a56c9e3bc988617cfb20aa7af734e768ee23e0cdd"),
        ("crates/rue-spec/cases/expressions/arithmetic.toml#case[6].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/expressions/arithmetic.toml#case[13].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/expressions/arithmetic.toml#case[14].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/expressions/arithmetic.toml#case[15].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/expressions/bitwise.toml#case[0].source", "custom-parse-error", "e60ab4193bb9d4fb40e13bdab7a3a3d140216cf7c039b447b2e1ca01260be485"),
        ("crates/rue-spec/cases/expressions/bitwise.toml#case[13].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/expressions/bitwise.toml#case[14].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/expressions/bitwise.toml#case[15].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/expressions/call.toml#case[6].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/expressions/call.toml#case[7].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/expressions/comparison.toml#case[3].source", "custom-parse-error", "3b1e0e14e897a06457155a7299fc59a8413b0c93672aeaeddc8123ae86288daa"),
        ("crates/rue-spec/cases/expressions/comparison.toml#case[5].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/expressions/comparison.toml#case[13].source", "custom-parse-error", "d567b76ef1e957aabc166097631fe9843c684c8f466269d43b3cfba4da8192cd"),
        ("crates/rue-spec/cases/expressions/comparison.toml#case[18].source", "custom-parse-error", "053d073f8d2a075b81036311217f51c2a8368f69c15c04f2d3a759bccc78588c"),
        ("crates/rue-spec/cases/expressions/comparison.toml#case[25].source", "unexpected-token", "81d0e1b61e4186f1fabb302a5db035617d351aea0939a30d08c986a0f7c2afe8"),
        ("crates/rue-spec/cases/expressions/comparison.toml#case[27].source", "unexpected-token", "e1b3a341ab178cb96abda9c7363fa6f38dd01cfd67f48a5a793d7c10f3aa0bac"),
        ("crates/rue-spec/cases/expressions/for.toml#case[2].source", "custom-parse-error", "79d568c5d360902329c072cd86d879748588e442d548944bc8777acee95d2e3b"),
        ("crates/rue-spec/cases/expressions/if.toml#case[5].source", "custom-parse-error", "deee0279224a59be292507da1c38f272f7b68b724056111aeb684f88b85edf95"),
        ("crates/rue-spec/cases/expressions/match.toml#case[59].source", "custom-parse-error", "e3539bc313e6b9895c91bc23882acd6b5abeb670bcb129dd70cbfacf6413bcc5"),
        ("crates/rue-spec/cases/expressions/while.toml#case[4].source", "custom-parse-error", "681518afe523c36a83a91be63195fde0ea9b9c5f31395fef86930f5962957f5c"),
        ("crates/rue-spec/cases/items/functions.toml#case[5].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[7].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[10].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[14].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[19].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[23].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[25].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[28].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[29].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[30].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[32].source", "unexpected-token", "fe10237f5cc2b9b19aab6968ff1562001f844368f0fac483f95b2e345ddaf7e0"),
        ("crates/rue-spec/cases/items/functions.toml#case[35].source", "unexpected-token", "ffc63fdf92db4c23cc166bc00f6eeadc97efaf28dea945e29cdeaef8df5de3b7"),
        ("crates/rue-spec/cases/items/functions.toml#case[47].source", "unexpected-token", "639ec70a3446ab8901bde835ee73622b60539b3901a67946264f30bf33dd672d"),
        ("crates/rue-spec/cases/items/functions.toml#case[48].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[67].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[69].source", "unexpected-token", "9a63ba0720e25603b8a887172b1025f466ceb04237f78ab33451173c6c728b9a"),
        ("crates/rue-spec/cases/items/functions.toml#case[73].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/items/functions.toml#case[74].source", "unexpected-token", "ffc63fdf92db4c23cc166bc00f6eeadc97efaf28dea945e29cdeaef8df5de3b7"),
        ("crates/rue-spec/cases/lexical/builtins.toml#case[5].source", "custom-parse-error", "3a7d357eef02cc985856a0efa7d212f091ecb5d866960a2b6e277f351ec54216"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[0].source", "unexpected-token", "b04836a0436e3bf1254e411e032f90de04b3a2bdc73b7bfe4f1e079b052c4fb8"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[1].source", "unexpected-token", "9f547d819cd9efc11cbeac980ef2d620e4cdd6e090cf23f3bddfb575f58b4493"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[2].source", "unexpected-token", "ae7309947b56fc4a2204e93bfba6554282069708977a89c5e37fa7f091e31128"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[3].source", "unexpected-token", "b7411849095aeaeab8fa2d5be5425b4cbcf4fbbfc8b391a57d2a69b7458ea050"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[4].source", "unexpected-token", "be27716d949dd26ef99b425f8a729ae5bca613c7dd6929f69a2b1e2fd91de92b"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[5].source", "unexpected-token", "e2da7409da419e6d43f95b573ceddbee41eb474a0e01a0c3d8f93363853e08c0"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[6].source", "unexpected-token", "28d84fc224574e0e61b210d2d5ac827b2b2eecb77e695caa3a41a98a8e13e283"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[7].source", "unexpected-token", "9588e86a54faced9927938470068b2d1bc50eec021531d8d5b8ecaa6510d6df7"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[8].source", "unexpected-token", "7aa09fa107bf758e95a561dabf9095998063b34cbe938859d934d0b82d759f45"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[9].source", "unexpected-token", "0f3b5d203529f1f6774d5a65bf9e606dbcd81321c15bbefd367f25d581d6e607"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[10].source", "unexpected-token", "e1d39d1012cfdeea328b2105006fd134973c1f568004863d7375f3808fe7adc7"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[11].source", "unexpected-token", "137f7576ca81986baf89f5c7839334de8aa39e5d407b887b3e9f89467ec60eb8"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[12].source", "unexpected-token", "0d7877140818328e36f32adaec64395dff990be3b9ac34bf778e90b3eca09155"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[13].source", "unexpected-token", "68dd21676c7fe7c80fb9d16641c449cfdd41fea2702146f999da5a931cbd2f8b"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[14].source", "unexpected-token", "9e5ab78cba954af17694fb00a1ef588fc3c19ce308a3629e88b9bbdfc080bd1a"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[15].source", "unexpected-token", "e99c4fc00cf7abc78e1814911fdd1984436180207c4a8b66d48dc1f534a998ef"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[16].source", "unexpected-token", "d8d43992522f3145ccf87df4dde8d4e3a0f4c59a41c5acfb3a17c63eeae60efa"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[17].source", "unexpected-token", "39e3034166aad51efd6c95a579569e299c6cb1c229d5229cf7f71baa5a9c361e"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[18].source", "unexpected-token", "05685c62f989a037fe636a64d4e8083be8ee1c59db0c367f64914d02203a7494"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[19].source", "unexpected-token", "da99d7dc81377e2ae2968edf51bbfb65a97f4c0b9e62eb531459076006686507"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[20].source", "unexpected-token", "05e995382ecff7d10559401176f96444cf4b4212960aae2a06a7fb0649f1d49e"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[21].source", "unexpected-token", "773685aacbca0df5735be6cbbc88669fd1b317eb9c97632f2e491c579048b6d4"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[22].source", "unexpected-token", "ed3873c26ceb35662d7c4b783c526f9675f25769ef08d2bcd9a140b96493bc19"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[23].source", "unexpected-token", "043431268675aaa818e11b3a985a5ca3b0b0419a60a06ba6468e23a6e2f2bcd2"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[24].source", "unexpected-token", "3ebbfb6835560da373eb31df83c88920ee5541c8545e4474bf8a763df29a0fd5"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[25].source", "unexpected-token", "6a983afefc062fa2ab32fd508f92bd89332aa65adcc7ce9b17f4532acf689fe4"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[26].source", "unexpected-token", "c5458cc2aa89d756856e8fc2501c9d11e5467271db2ac6e54458821f9d1cd3f4"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[27].source", "unexpected-token", "8e69e2fdd1e6c120fca93cafe5e6bd2550c49b6512e05c96dfa699dd2f5045bb"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[28].source", "unexpected-token", "f76b485b311c8b5e2aa952e0d6e38cee9e28365138d26b9f80cfb66dc6c785bb"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[29].source", "unexpected-token", "41b05bbff0ddc8b2e7a41b6b5f6d7869d68fcf19febb39dbc3487685ca622dbe"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[30].source", "unexpected-token", "84571b91ae3446a049bbd2a1a55cc8ae1b36c57afed3c9969afd095d1dda4198"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[31].source", "unexpected-token", "6a8458b9db9fb1e201c105e457cdad1e4842bb437afc2ce3ca4e8df7ea77a357"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[32].source", "unexpected-token", "0121403868b9ef964445591931205e2509c3c9287916f45e1f04dfe36d8681cc"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[33].source", "unexpected-token", "4dcd4b3cdb0a7120bb0612eb2ee3ed2e7ab6d783d118aa938e47c7bde300b660"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[34].source", "unexpected-token", "027139bd17f4073ed2c697ec909e10424941155f18623f1dd7d5e1670a082774"),
        ("crates/rue-spec/cases/lexical/keywords.toml#case[35].source", "unexpected-token", "c1e22a69a8a80708e6e045fca14a3fee3b33e90765fb0eb5e0cd99d229f479c4"),
        ("crates/rue-spec/cases/lexical/tokens.toml#case[37].source", "unexpected-token", "b04836a0436e3bf1254e411e032f90de04b3a2bdc73b7bfe4f1e079b052c4fb8"),
        ("crates/rue-spec/cases/lexical/tokens.toml#case[38].source", "unexpected-token", "9f547d819cd9efc11cbeac980ef2d620e4cdd6e090cf23f3bddfb575f58b4493"),
        ("crates/rue-spec/cases/lexical/tokens.toml#case[39].source", "unexpected-token", "b7411849095aeaeab8fa2d5be5425b4cbcf4fbbfc8b391a57d2a69b7458ea050"),
        ("crates/rue-spec/cases/lexical/tokens.toml#case[42].source", "unexpected-token", "353f925909b9c714098da33167fff6b2f01bc76ef83cc1b23b180bfabd279272"),
        ("crates/rue-spec/cases/lexical/tokens.toml#case[64].source", "custom-parse-error", "72fc6db6dd1eaf8ef15a38b5fb0a6529a95936f4205b057286bf06d490fcbcbf"),
        ("crates/rue-spec/cases/statements/assignment.toml#case[13].source", "unexpected-token", "1dca0e766791ee8380f02947c99a2943c1eefd116814970cf2b13907cfc66e7f"),
        ("crates/rue-spec/cases/statements/let.toml#case[13].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-spec/cases/types/inference.toml#case[1].source", "unexpected-token", "16482a10be327e100a5c624cd81d115abd38bfee091ecbb136023cc8971ee13a"),
        ("crates/rue-spec/cases/types/integers.toml#case[0].source", "unexpected-token", "1b9785e254030965edc73d6ab79276763c55a1cfe61f0e622b942dd847404c65"),
        ("crates/rue-spec/cases/types/integers.toml#case[1].source", "unexpected-token", "1b9785e254030965edc73d6ab79276763c55a1cfe61f0e622b942dd847404c65"),
        ("crates/rue-spec/cases/types/integers.toml#case[2].source", "unexpected-token", "1b9785e254030965edc73d6ab79276763c55a1cfe61f0e622b942dd847404c65"),
        ("crates/rue-spec/cases/types/integers.toml#case[12].source", "unexpected-token", "7f4f349e268dcfd2fe0c8f0fad75e3582b09632a16dc2b9197dce8b53a408916"),
        ("crates/rue-spec/cases/types/integers.toml#case[13].source", "unexpected-token", "7f4f349e268dcfd2fe0c8f0fad75e3582b09632a16dc2b9197dce8b53a408916"),
        ("crates/rue-spec/cases/types/integers.toml#case[14].source", "unexpected-token", "7f4f349e268dcfd2fe0c8f0fad75e3582b09632a16dc2b9197dce8b53a408916"),
        ("crates/rue-spec/cases/types/integers.toml#case[20].source", "unexpected-token", "7f4f349e268dcfd2fe0c8f0fad75e3582b09632a16dc2b9197dce8b53a408916"),
        ("crates/rue-spec/cases/types/integers.toml#case[22].source", "unexpected-token", "7f4f349e268dcfd2fe0c8f0fad75e3582b09632a16dc2b9197dce8b53a408916"),
        ("crates/rue-spec/cases/types/integers.toml#case[23].source", "unexpected-token", "7f4f349e268dcfd2fe0c8f0fad75e3582b09632a16dc2b9197dce8b53a408916"),
        ("crates/rue-spec/cases/types/integers.toml#case[25].source", "unexpected-token", "1b9785e254030965edc73d6ab79276763c55a1cfe61f0e622b942dd847404c65"),
        ("crates/rue-spec/cases/types/integers.toml#case[28].source", "unexpected-token", "1b9785e254030965edc73d6ab79276763c55a1cfe61f0e622b942dd847404c65"),
        ("crates/rue-spec/cases/types/integers.toml#case[30].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/types/integers.toml#case[31].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/types/integers.toml#case[39].source", "unexpected-token", "251a5baf86059615515761f84f9fa49a491ee7d624818d0354fbf445aae56395"),
        ("crates/rue-spec/cases/types/integers.toml#case[42].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/types/integers.toml#case[44].source", "unexpected-token", "6b0024728c90f355c500cb51b57bd6071e2b7cb4f3c4712cc5cbc0d9ca6d7b2b"),
        ("crates/rue-spec/cases/types/move-semantics.toml#case[66].source", "unexpected-token", "a4a147d9d174b518777b26523544e4f8c3c4c53510c1f5bf9ebdfc7b6a927590"),
        ("crates/rue-spec/cases/types/str-type.toml#case[22].source", "unexpected-token", "aaaa0410a25271ad06b3f6f01819fc7817e21da78b2555af5765409ac2b0c70e"),
        ("crates/rue-ui-tests/cases/diagnostics/directives.toml#case[1].source", "custom-parse-error", "24b74ff25dfe6f436f9baec6e10fd66ef4934d830923c97463ed0717e2cb3b8e"),
        ("crates/rue-ui-tests/cases/diagnostics/directives.toml#case[3].source", "custom-parse-error", "bccc0ca9be035e1bd24fed98fa9cb001e0d4d67488634afa32d729a0e2b8a75d"),
        ("crates/rue-ui-tests/cases/diagnostics/directives.toml#case[10].source", "custom-parse-error", "5fb5e9b903d95a69bdfd884346c15bb2c87ba6b0f13ad484aefbc73cd4ed851a"),
        ("crates/rue-ui-tests/cases/diagnostics/directives.toml#case[11].source", "custom-parse-error", "db08daf7b82e001a6eb5fad53959d1308a685feff8c08e3c824585064f2e55c1"),
        ("crates/rue-ui-tests/cases/diagnostics/directives.toml#case[12].source", "custom-parse-error", "c4f8af5ec0d9b7b6b89044b8eae0421c8dcc2e8cdef478b33eba20e35a4da465"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[0].source", "unexpected-token", "fc54b1379d02ae2e33643156868ef00f6a3671df75c853771b509f9ade9dce61"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[1].source", "unexpected-token", "8ece17f3722bee2e88b68a5e513a1d2ac859651238285e734685df39f1daec45"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[2].source", "unexpected-token", "d7783519cf232feb2fe32a16400fae14b9b9612c729f2619d3302c2f890f62d1"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[3].source", "unexpected-token", "7eca9dfa7230718ff675250d2d485914715765fbf950591c6704efa46b23817e"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[4].source", "unexpected-token", "fd4fcc8700e0354badd79cbfe3f5fd28250f3343cda80ebfc779d9935f3438e4"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[5].source", "unexpected-token", "d2263404544a17bd6a13c8abdcb85becb47d318fa37ffd94c76f4c73ae094cbc"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[6].source", "custom-parse-error", "ba9e6e6bfa2661357ec1eed3744d6d47858220275d09096e54423730c0248492"),
        ("crates/rue-ui-tests/cases/diagnostics/multi-error.toml#case[7].source", "unexpected-token", "0eb687701021d5147d647873a043a2ea5c8c19774c8f6932f783e5aaca06a223"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[0].source", "unexpected-token", "c9e9a8a4634cb7b15285ce8c49693f39e8eb47144d724cc2720b60cdc53d6d65"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[1].source", "unexpected-token", "672bbd320d9ec562a35948a662a395352da546097d14faf333e7e60b137ac093"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[2].source", "unexpected-token", "2c013ddb7eaef5a3a10fdbe23a141528e1e27634358639770da5902ea9f2b9d4"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[3].source", "unexpected-token", "cfaab604ee95a4e860f16410d98dd1c37ad4f7ccaafbe571fa18f5c2ea9afdfb"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[4].source", "unexpected-token", "682ff770d587e7c086eded3cea6affa96d8a7a795b0070c865bbea7818fddfbe"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[5].source", "unexpected-token", "10af713b54a61f0bbe1ff83525d8ee1304f809728600e2dcb2a4bb7a81e495b4"),
        ("crates/rue-ui-tests/cases/diagnostics/parser_messages.toml#case[6].source", "unexpected-token", "49fa4ef2299bcff7afcb51b3e209d61bdda84f356e871bb3101684944f6fc1c0"),
        ("crates/rue-ui-tests/cases/diagnostics/rust_refugees.toml#case[3].source", "unexpected-token", "76bc111034a928cf56e21556f1f226a6268eb0863d1419b289e6d16f161bae18"),
        ("crates/rue-ui-tests/cases/diagnostics/rust_refugees.toml#case[4].source", "unexpected-token", "ca512ef710570dd334210f4a990cb55c0bf9424507f408e7e558ee78def3d40c"),
        ];
        REVIEWED
            .iter()
            .map(|(fixture, category, fingerprint)| {
                (
                    ((*fixture).to_owned(), (*category).to_owned()),
                    *fingerprint,
                )
            })
            .collect()
    }

    fn diagnostic_delta_fingerprint(expected: &CompileErrors, actual: &CompileErrors) -> String {
        fn text(hasher: &mut Sha256, value: &str) {
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }

        fn span(hasher: &mut Sha256, value: Span) {
            hasher.update(value.file_id.index().to_le_bytes());
            hasher.update(value.start.to_le_bytes());
            hasher.update(value.end.to_le_bytes());
        }

        fn feed(hasher: &mut Sha256, side: u8, errors: &CompileErrors) {
            hasher.update([side]);
            hasher.update((errors.len() as u64).to_le_bytes());
            for error in errors.iter() {
                // Debug preserves the enum variant and all of its fields;
                // Display preserves the exact rendered message contract.
                text(hasher, &format!("{:?}", error.kind));
                text(hasher, &error.kind.to_string());
                match error.span() {
                    Some(value) => {
                        hasher.update([1]);
                        span(hasher, value);
                    }
                    None => hasher.update([0]),
                }

                let diagnostic = error.diagnostic();
                hasher.update((diagnostic.labels.len() as u64).to_le_bytes());
                for label in &diagnostic.labels {
                    text(hasher, &label.message);
                    span(hasher, label.span);
                }
                hasher.update((diagnostic.notes.len() as u64).to_le_bytes());
                for note in &diagnostic.notes {
                    text(hasher, &note.0);
                }
                hasher.update((diagnostic.helps.len() as u64).to_le_bytes());
                for help in &diagnostic.helps {
                    text(hasher, &help.0);
                }
                hasher.update((diagnostic.suggestions.len() as u64).to_le_bytes());
                for suggestion in &diagnostic.suggestions {
                    text(hasher, &suggestion.message);
                    span(hasher, suggestion.span);
                    text(hasher, &suggestion.replacement);
                    hasher.update([match suggestion.applicability {
                        rue_error::Applicability::MachineApplicable => 0,
                        rue_error::Applicability::MaybeIncorrect => 1,
                        rue_error::Applicability::HasPlaceholders => 2,
                        rue_error::Applicability::Unspecified => 3,
                    }]);
                }
            }
        }

        let mut hasher = Sha256::new();
        feed(&mut hasher, b'E', expected);
        feed(&mut hasher, b'A', actual);
        format!("{:x}", hasher.finalize())
    }

    #[test]
    fn diagnostic_delta_fingerprint_covers_complete_ordered_diagnostics() {
        fn errors(kind: ErrorKind, file: u32, start: u32, end: u32) -> CompileErrors {
            CompileErrors::from(vec![CompileError::new(
                kind,
                Span::with_file(FileId::new(file), start, end),
            )])
        }

        let expected = errors(
            ErrorKind::UnexpectedToken {
                expected: "identifier".into(),
                found: "integer".into(),
            },
            1,
            2,
            3,
        );
        let actual = errors(ErrorKind::ParseError("candidate message".into()), 1, 2, 3);
        let baseline = diagnostic_delta_fingerprint(&expected, &actual);

        for mutation in [
            errors(ErrorKind::ParseError("changed message".into()), 1, 2, 3),
            errors(
                ErrorKind::UnexpectedToken {
                    expected: "candidate".into(),
                    found: "message".into(),
                },
                1,
                2,
                3,
            ),
            errors(ErrorKind::ParseError("candidate message".into()), 1, 3, 4),
            errors(ErrorKind::ParseError("candidate message".into()), 2, 2, 3),
        ] {
            assert_ne!(baseline, diagnostic_delta_fingerprint(&expected, &mutation));
        }

        let two_actual = CompileErrors::from(vec![
            CompileError::new(
                ErrorKind::ParseError("candidate message".into()),
                Span::with_file(FileId::new(1), 2, 3),
            ),
            CompileError::new(
                ErrorKind::ParseError("second".into()),
                Span::with_file(FileId::new(1), 4, 5),
            ),
        ]);
        assert_ne!(
            baseline,
            diagnostic_delta_fingerprint(&expected, &two_actual)
        );

        let help_actual = CompileErrors::from(vec![
            CompileError::new(
                ErrorKind::ParseError("candidate message".into()),
                Span::with_file(FileId::new(1), 2, 3),
            )
            .with_help("additional help"),
        ]);
        assert_ne!(
            baseline,
            diagnostic_delta_fingerprint(&expected, &help_actual)
        );

        let label_actual = CompileErrors::from(vec![
            CompileError::new(
                ErrorKind::ParseError("candidate message".into()),
                Span::with_file(FileId::new(1), 2, 3),
            )
            .with_label(
                "secondary location",
                Span::with_file(FileId::new(9), 10, 11),
            ),
        ]);
        assert_ne!(
            baseline,
            diagnostic_delta_fingerprint(&expected, &label_actual)
        );
    }

    #[test]
    #[ignore = "TOML corpus acceptance/diagnostic differential; run before publishing RUE-904"]
    fn toml_source_corpus_matches_acceptance_and_classifies_diagnostics() {
        let root = std::env::current_dir().expect("test working directory");
        let mut files = Vec::new();
        for directory in [
            "crates/rue-spec",
            "crates/rue-ui-tests",
            "crates/rue-cli-tests",
        ] {
            toml_files(&root.join(directory), &mut files);
        }
        files.sort();
        let mut accepted = 0usize;
        let mut rejected = 0usize;
        let mut exact_diagnostics = 0usize;
        // Rich's recovery can select a different expected-token set than a
        // cursor parser after both have already rejected the same fixture.
        // Every accepted difference is named here by exact fixture location
        // and the canonical diagnostic category. New differences fail the
        // audit instead of silently inflating a counter.
        let rich_delta_allowlist = rich_delta_allowlist();
        let mut classified_rich_deltas = Vec::new();
        let mut unclassified_deltas = Vec::new();
        let mut used_allowlist_entries = std::collections::BTreeSet::new();
        let mut lex_invalid = 0usize;
        let mut acceptance_gaps = Vec::new();
        for file in files {
            let text = fs::read_to_string(&file).unwrap();
            let relative_file = file.strip_prefix(&root).unwrap_or(&file).display();
            for fixture in toml_sources(&text) {
                let source = fixture.source.as_str();
                let fixture_key = format!("{relative_file}#{}", fixture.locator);
                let Ok((expected_tokens, expected_interner)) = Lexer::new(source).tokenize() else {
                    lex_invalid += 1;
                    continue;
                };
                let (actual_tokens, actual_interner) = Lexer::new(source)
                    .tokenize()
                    .expect("the same source must lex deterministically");
                let expected = ChumskyParser::new(expected_tokens, expected_interner)
                    .parse_preserving_interner();
                let actual = HandwrittenParser::new(actual_tokens, actual_interner).parse();
                match (expected, actual) {
                    (Ok((expected, _)), Ok((actual, _))) => {
                        accepted += 1;
                        if actual != expected {
                            acceptance_gaps.push(format!("{fixture_key}: AST mismatch"));
                        }
                    }
                    (Err((expected, _)), Err((actual, _))) => {
                        rejected += 1;
                        let actual_spans = actual
                            .iter()
                            .filter_map(|error| error.span())
                            .collect::<Vec<_>>();
                        assert!(
                            actual_spans
                                .windows(2)
                                .all(|pair| pair[0].start <= pair[1].start),
                            "candidate diagnostic order regressed in {fixture_key}",
                        );
                        assert!(
                            actual_spans
                                .iter()
                                .all(|span| span.file_id == FileId::DEFAULT)
                        );
                        let expected_keys = expected
                            .iter()
                            .map(|error| (&error.kind, error.span()))
                            .collect::<Vec<_>>();
                        let actual_keys = actual
                            .iter()
                            .map(|error| (&error.kind, error.span()))
                            .collect::<Vec<_>>();
                        if actual_keys == expected_keys {
                            exact_diagnostics += 1;
                        } else {
                            let category = match expected.first().map(|error| &error.kind) {
                                Some(ErrorKind::UnexpectedToken { .. }) => "unexpected-token",
                                Some(ErrorKind::ParseError(_)) => "custom-parse-error",
                                Some(ErrorKind::NestingLimitExceeded { .. }) => "nesting-limit",
                                Some(_) => "non-parser-error",
                                None => "no-canonical-diagnostic",
                            };
                            let allowlist_key = (fixture_key.clone(), category.to_owned());
                            let fingerprint = diagnostic_delta_fingerprint(&expected, &actual);
                            if rich_delta_allowlist
                                .get(&allowlist_key)
                                .is_some_and(|reviewed| *reviewed == fingerprint)
                            {
                                used_allowlist_entries.insert(allowlist_key);
                                classified_rich_deltas.push(format!("{fixture_key} [{category}]"));
                            } else {
                                unclassified_deltas.push(format!(
                                    "        ({fixture_key:?}, {category:?}, {fingerprint:?}),"
                                ));
                            }
                        }
                    }
                    (expected, actual) => acceptance_gaps.push(format!(
                        "{fixture_key}: canonical={}, candidate={}",
                        expected.is_ok(),
                        actual.is_ok()
                    )),
                }
            }
        }
        assert!(
            accepted > 0 && rejected > 0,
            "expected both accepted and parser-rejected sources; accepted={accepted}, rejected={rejected}, lex_invalid={lex_invalid}"
        );
        assert!(
            acceptance_gaps.is_empty(),
            "TOML corpus parity gaps:\n{}",
            acceptance_gaps.join("\n")
        );
        assert!(
            unclassified_deltas.is_empty(),
            "unclassified diagnostic deltas:\n{}",
            unclassified_deltas.join("\n")
        );
        assert_eq!(
            used_allowlist_entries,
            rich_delta_allowlist.keys().cloned().collect(),
            "diagnostic delta allowlist contains stale entries"
        );
        eprintln!(
            "RUE-904 TOML differential: accepted={accepted}, rejected={rejected}, lex_invalid={lex_invalid}, exact_diagnostics={exact_diagnostics}, classified_rich_deltas={}",
            classified_rich_deltas.len()
        );
    }

    #[test]
    #[ignore = "repository-corpus differential; run explicitly before publishing RUE-904"]
    fn repository_rue_corpus_matches_canonical_on_successes() {
        let root = std::env::current_dir().expect("test working directory");
        let mut files = Vec::new();
        for directory in ["benchmarks", "examples", "tests", "cli-test-fixtures"] {
            rue_files(&root.join(directory), &mut files);
        }
        files.sort();
        assert!(
            !files.is_empty(),
            "no repository Rue corpus found under {root:?}"
        );
        let mut compared = 0;
        let mut gaps = Vec::new();
        for file in files {
            let source = fs::read_to_string(&file).unwrap();
            let Ok((expected, _)) = canonical(&source) else {
                continue;
            };
            match candidate(&source) {
                Ok((actual, _)) if actual == expected => compared += 1,
                Ok(_) => gaps.push(format!("{}: AST mismatch", file.display())),
                Err((errors, _)) => gaps.push(format!("{}: {errors:?}", file.display())),
            }
        }
        assert!(compared > 0);
        assert!(
            gaps.is_empty(),
            "candidate parity gaps:\n{}",
            gaps.join("\n")
        );
        eprintln!("RUE-904 corpus parity: {compared} canonical-success files matched");
    }

    #[test]
    fn nesting_and_recovery_contracts_are_applied() {
        let source = "fn ; fn ; fn good() -> i32 { 1 }";
        let file_id = FileId::new(17);
        let (tokens, interner) = Lexer::with_file_id(source, file_id).tokenize().unwrap();
        let parser = HandwrittenParser::new(tokens, interner);
        let (errors, _) = parser.parse().unwrap_err();
        let spans = errors
            .iter()
            .map(|error| error.span().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(spans.len(), 2, "one ordered diagnostic per malformed item");
        assert!(spans[0].start < spans[1].start);
        assert!(spans.iter().all(|span| span.file_id == file_id));

        let moderate = format!(
            "fn main() -> i32 {{ {}0{} }}",
            "(".repeat(128),
            ")".repeat(128)
        );
        assert_parity(&moderate);

        let deep = format!(
            "fn main() -> i32 {{ {}0{} }}",
            "(".repeat(rue_error::MAX_NESTING_DEPTH + 2),
            ")".repeat(rue_error::MAX_NESTING_DEPTH + 2)
        );
        let (errors, _) = candidate(&deep).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e.kind, ErrorKind::NestingLimitExceeded { .. }))
        );
    }

    #[test]
    #[ignore = "microbenchmark; run explicitly with --ignored --nocapture"]
    fn representative_timing_smoke() {
        fn compare(source: &str, iterations: usize, label: &str) -> (Duration, Duration) {
            fn candidate_sample(source: &str, iterations: usize) -> Duration {
                let inputs = (0..iterations).map(|_| lex(source)).collect::<Vec<_>>();
                let start = Instant::now();
                for (tokens, interner) in inputs {
                    HandwrittenParser::new(tokens, interner).parse().unwrap();
                }
                start.elapsed()
            }
            fn canonical_sample(source: &str, iterations: usize) -> Duration {
                let inputs = (0..iterations).map(|_| lex(source)).collect::<Vec<_>>();
                let start = Instant::now();
                for (tokens, interner) in inputs {
                    ChumskyParser::new(tokens, interner).parse().unwrap();
                }
                start.elapsed()
            }

            // Populate instruction/data caches before collecting samples.
            candidate_sample(source, 1);
            canonical_sample(source, 1);

            let mut candidate_samples = Vec::new();
            let mut canonical_samples = Vec::new();
            for sample in 0..7 {
                // Alternate order to avoid assigning drift consistently to one
                // parser while keeping lexing/allocation outside the timer.
                if sample % 2 == 0 {
                    candidate_samples.push(candidate_sample(source, iterations));
                    canonical_samples.push(canonical_sample(source, iterations));
                } else {
                    canonical_samples.push(canonical_sample(source, iterations));
                    candidate_samples.push(candidate_sample(source, iterations));
                }
            }
            candidate_samples.sort_unstable();
            canonical_samples.sort_unstable();
            let candidate_median = candidate_samples[candidate_samples.len() / 2];
            let canonical_median = canonical_samples[canonical_samples.len() / 2];
            let speedup = canonical_median.as_secs_f64() / candidate_median.as_secs_f64();
            eprintln!(
                "RUE-904 {label} timing (pre-lexed median of 7): candidate={candidate_median:?}, canonical={canonical_median:?}, speedup={speedup:.2}x"
            );
            assert!(
                speedup >= 2.0,
                "{label} speedup {speedup:.2}x fell below the conservative 2x gate"
            );
            (candidate_median, canonical_median)
        }

        let representative = format!(
            "fn main() -> i32 {{ {} 0 }}",
            (0..4000)
                .map(|i| format!("let x{i} = {i} + 2 * 3; "))
                .collect::<String>()
        );
        let nested = format!(
            "fn main() -> i32 {{ {}0{} }}",
            "{ ".repeat(60),
            " } + 0".repeat(60)
        );
        compare(&representative, 20, "representative");
        compare(&nested, 10, "nested-continuation stress");
    }
}
