//! Pratt expressions, postfix operations, calls, intrinsics, and aggregates.

use super::*;

impl Parser {
    pub(super) fn expr(&mut self) -> PResult<Expr> {
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

    pub(super) fn pratt_tail(&mut self, lhs: Expr, min_bp: u8) -> PResult<Expr> {
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

    pub(super) fn primary(&mut self) -> PResult<Expr> {
        let start = self.start();
        match self.kind() {
            TokenKind::Int(value) => {
                let span = self.bump().span;
                Ok(Expr::Int(IntLit { value, span }))
            }
            TokenKind::Float(value) => {
                let span = self.bump().span;
                Ok(Expr::Float(FloatLit { value, span }))
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
            TokenKind::Yield => {
                self.bump();
                // Unlike `return`, the operand is mandatory: an accessor
                // always hands out a place (ADR-0062).
                let value = Box::new(self.expr()?);
                Ok(Expr::Yield(YieldExpr {
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
                // The only two production sites for value-position anonymous
                // type literals; the enclosing declaration reads the counter to
                // decide whether its body needs an anonymous-site walk
                // (RUE-1837).
                self.anonymous_type_literals += 1;
                let type_expr = self.anonymous_struct_type(true)?;
                Ok(Expr::TypeLit(TypeLitExpr {
                    span: type_expr.span(),
                    type_expr,
                }))
            }
            TokenKind::Enum => {
                self.anonymous_type_literals += 1;
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

    /// Reject a trailing-dot float literal (`5.`) before it is mistaken for a
    /// member access with a missing name (ADR-0065 §3, RUE-1068).
    ///
    /// The lexer cannot make this call: `42.` is also the prefix of the legal
    /// method call `42.to_string()`, and logos commits to the longest match
    /// without lookahead, so a lexical `[0-9]+\.` rule would swallow the
    /// receiver of every method call on an integer literal. Here the decision
    /// is easy — the literal must be directly followed by the `.` (no space),
    /// and the token after the `.` must not be a member name.
    ///
    /// The diagnostic is [`ErrorKind::MalformedFloatLiteral`] (E0011), the same
    /// kind the lexer uses for the leading-dot form, because both diagnose one
    /// spelling rule; only the phase that can decide it differs.
    fn reject_trailing_dot_float(&mut self, base: &Expr) -> PResult<()> {
        if !matches!(base, Expr::Int(_) | Expr::Float(_)) {
            return Ok(());
        }
        let dot_span = self.tokens[self.cursor].span;
        let adjacent = base.span().end == dot_span.start;
        let member_follows = matches!(self.nth(1), TokenKind::Ident(_));
        if !adjacent || member_follows {
            return Ok(());
        }
        let literal_text = match base {
            Expr::Int(literal) => literal.value.to_string(),
            Expr::Float(literal) => self.interner.resolve(&literal.value).to_string(),
            _ => unreachable!("literal kind was checked above"),
        };
        let message = match base {
            Expr::Int(_) => format!(
                "floating-point literal cannot end with `.`: write `{literal_text}.0` instead \
                 of `{literal_text}.`"
            ),
            Expr::Float(_) => format!(
                "floating-point literal cannot end with `.`: write `{literal_text}` instead \
                 of `{literal_text}.`"
            ),
            _ => unreachable!("literal kind was checked above"),
        };
        let span = base.span().extend_to(dot_span.end);
        self.record_error(CompileError::new(
            ErrorKind::MalformedFloatLiteral(message),
            span,
        ));
        Err(())
    }

    fn postfix(&mut self, mut base: Expr) -> PResult<Expr> {
        loop {
            match self.kind() {
                TokenKind::Dot => {
                    self.reject_trailing_dot_float(&base)?;
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

    pub(super) fn call_args(&mut self) -> PResult<Vec<CallArg>> {
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if self.at(TokenKind::Comma) && self.nth(1) == TokenKind::RParen {
            let comma = self.bump();
            self.error_at("expected argument before `,`", comma.span);
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
                self.bump();
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
                            // `ident_expected` already reports the actual
                            // offending token. Repeating the failure at `@`
                            // obscures that location and would violate the
                            // parser's source-span invariant.
                            return Err(());
                        }
                    }
                }
            }
            _ => unreachable!(),
        };
        self.expect(TokenKind::LParen)?;
        // Type-position intrinsics (`@size_of`, `@align_of`, `@offset_of`'s
        // first argument, ...) take the canonical type grammar, exactly as
        // annotations do (RUE-788). Every other intrinsic takes the expression
        // grammar, with a narrow carve-out for argument tokens that can only
        // spell a type; AstGen preserves those as placeholders so semantic
        // analysis reports the type-vs-value mismatch at the right arity.
        let type_positions = {
            let name_str = self.interner.resolve(&name.name);
            crate::intrinsics::type_argument_count(name_str)
        };
        let mut args = Vec::new();
        if !self.at(TokenKind::RParen) {
            loop {
                if args.len() < type_positions {
                    match self.type_position_intrinsic_arg(name) {
                        Ok(ty) => args.push(IntrinsicArg::Type(ty)),
                        // The targeted diagnostic is already recorded (and any
                        // recorded error fails the parse), so resynchronize at
                        // the argument boundary. Keeping the enclosing call
                        // structurally intact stops item-level recovery from
                        // re-parsing the `@name(...)` as a directive and
                        // cascading a second, misleading diagnostic.
                        Err(()) => self.skip_type_position_argument(),
                    }
                } else {
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
        if name.name == self.syms.allow_directive
            || name.name == self.syms.copy_directive
            || name.name == self.syms.repr_directive
        {
            self.error_at("directive must precede a statement", self.span_from(start));
            return Err(());
        }
        Ok(Expr::IntrinsicCall(IntrinsicCallExpr {
            name,
            args,
            span: self.span_from(start),
        }))
    }

    /// Parse one type-position intrinsic argument with the canonical type
    /// grammar (`ty()`), so type intrinsics accept every `TypeExpr` form an
    /// annotation accepts: pointer, qualified, type-call, array, slice,
    /// anonymous aggregate, unit, never, and primitive types (RUE-788).
    ///
    /// Expression-shaped mistakes get one targeted diagnostic here instead of
    /// drifting through expression parsing: a `!` with an operand is a
    /// prefix-not expression (only a bare `!` spells the never type), and a
    /// trailing token after a complete type (`i32 + 1`, `Point { .. }`) means
    /// the argument was a value expression.
    fn type_position_intrinsic_arg(&mut self, intrinsic: Ident) -> PResult<TypeExpr> {
        let intrinsic_name = self.interner.resolve(&intrinsic.name).to_string();
        if self.at(TokenKind::Bang) && !matches!(self.nth(1), TokenKind::Comma | TokenKind::RParen)
        {
            self.error(format!(
                "`@{intrinsic_name}` takes a type in this argument position, but `!` followed by \
                 an operand is a prefix-not expression; write a bare `!` for the never type"
            ));
            return Err(());
        }
        let ty = self.ty()?;
        if !self.at(TokenKind::Comma) && !self.at(TokenKind::RParen) {
            self.error(format!(
                "`@{intrinsic_name}` takes a type in this argument position, not a value \
                 expression"
            ));
            return Err(());
        }
        Ok(ty)
    }

    /// Skip past a malformed type-position argument to its boundary: the
    /// argument-separating `,` or the closing `)` at the intrinsic call's own
    /// nesting level (or end of input while unterminated).
    fn skip_type_position_argument(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.kind() {
                TokenKind::Eof => return,
                TokenKind::Comma | TokenKind::RParen if depth == 0 => return,
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            self.bump();
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
}

pub(super) fn binary_binding(kind: TokenKind) -> Option<(u8, u8, BinaryOp)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn parses(source: &str) -> bool {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner).parse().is_ok()
    }

    #[test]
    fn parses_precedence_postfix_calls_and_aggregates() {
        assert!(parses(
            "fn f(xs: [i32], x: i32) -> i32 { g(x + 2 * 3).field + xs[0] + [x, 2][1] }"
        ));
    }

    #[test]
    fn rejects_an_unclosed_call_argument_list() {
        assert!(!parses("fn f() { g(1, 2; }"));
    }

    /// Parse `source` and return the final expression of `f`'s body together
    /// with the interner, so the float tests can inspect the literal's text.
    fn tail_expr(source: &str) -> (Expr, lasso::ThreadedRodeo) {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner)
            .parse()
            .unwrap_or_else(|errors| panic!("`{source}` should parse: {errors:?}"));
        let crate::ast::Item::Function(function) = &ast.items[0] else {
            panic!("expected the first item to be a function");
        };
        let Expr::Block(body) = &function.body else {
            panic!("expected a block body");
        };
        (body.expr.as_ref().clone(), interner)
    }

    fn parse_errors(source: &str) -> Vec<String> {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        match Parser::new(tokens, interner).parse() {
            Ok(_) => Vec::new(),
            Err(errors) => errors.iter().map(|error| error.to_string()).collect(),
        }
    }

    fn parse_error_spans(source: &str) -> Vec<(String, Span)> {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        match Parser::new(tokens, interner).parse() {
            Ok(_) => Vec::new(),
            Err(errors) => errors
                .iter()
                .map(|error| (error.to_string(), error.span().unwrap()))
                .collect(),
        }
    }

    #[test]
    fn empty_call_argument_is_diagnosed_at_the_comma_for_every_call_form() {
        let sources = [
            "fn f() { g(,); }",
            "fn f(s: S) { s.m(,); }",
            "fn f() { Pair(,) { field: 1 } }",
            "fn f() { module.Pair(,) { field: 1 } }",
            "fn f(x: i32) -> i32 { match x { Result(,).Ok(v) => v } }",
            "fn f(x: i32) -> i32 { match x { module.Result(,).Ok(v) => v } }",
        ];

        for source in sources {
            let errors = parse_error_spans(source);
            let comma = source.find(',').unwrap() as u32;
            assert_eq!(
                errors,
                vec![(
                    "expected argument before `,`".to_string(),
                    Span::new(comma, comma + 1),
                )],
                "unexpected diagnostics for `{source}`"
            );
        }
    }

    #[test]
    fn empty_call_argument_recovers_to_later_diagnostics() {
        assert_eq!(
            parse_errors("fn f() -> i32 { g(,); let x = ; 0 }"),
            vec![
                "expected argument before `,`",
                "expected expression, found ';'",
            ]
        );
    }

    #[test]
    fn empty_and_trailing_call_argument_lists_remain_legal() {
        assert!(parses("fn f() { g(); }"));
        assert!(parses("fn f() { g(1,); }"));
        assert!(parses("fn f() { if g() { g(1,) } }"));
    }

    #[test]
    fn float_literal_parses_in_expression_position() {
        // ADR-0065 §3 / RUE-1069: the literal reaches the AST untyped, holding
        // the text the lexer interned rather than a decoded value.
        let (expr, interner) = tail_expr("fn f() { 6.022e23 }");
        let Expr::Float(literal) = expr else {
            panic!("expected a float literal, got {expr:?}");
        };
        assert_eq!(interner.resolve(&literal.value), "6.022e23");
        assert_eq!((literal.span.start, literal.span.end), (9, 17));
    }

    #[test]
    fn float_literal_binds_like_any_other_primary() {
        // `1.5e-3 + x` is `(1.5e-3) + x` — the `-` belongs to the exponent, so
        // there is no subtraction here at all.
        let (expr, interner) = tail_expr("fn f(x: i32) { 1.5e-3 + x }");
        let Expr::Binary(binary) = expr else {
            panic!("expected a binary expression, got {expr:?}");
        };
        assert_eq!(binary.op, BinaryOp::Add);
        let Expr::Float(literal) = binary.left.as_ref() else {
            panic!("expected the left operand to be a float literal");
        };
        assert_eq!(interner.resolve(&literal.value), "1.5e-3");
        assert!(matches!(binary.right.as_ref(), Expr::Ident(_)));
    }

    #[test]
    fn float_literal_obeys_multiplicative_precedence() {
        // `1.5 * x + y` groups as `(1.5 * x) + y`.
        let (expr, _) = tail_expr("fn f(x: i32, y: i32) { 1.5 * x + y }");
        let Expr::Binary(add) = expr else {
            panic!("expected a binary expression, got {expr:?}");
        };
        assert_eq!(add.op, BinaryOp::Add);
        let Expr::Binary(mul) = add.left.as_ref() else {
            panic!("expected the left operand to be the multiplication");
        };
        assert_eq!(mul.op, BinaryOp::Mul);
        assert!(matches!(mul.left.as_ref(), Expr::Float(_)));
    }

    #[test]
    fn trailing_dot_float_is_rejected_by_the_parser() {
        // The lexer cannot decide this one — `5.` is also the prefix of
        // `5.to_string()` — so the parser reports it once no member name
        // follows the dot (ADR-0065 §3).
        assert_eq!(
            parse_errors("fn f() { let x = 5.; }"),
            vec!["floating-point literal cannot end with `.`: write `5.0` instead of `5.`"]
        );
    }

    #[test]
    fn trailing_dot_float_uses_the_float_without_its_final_dot() {
        for (source, expected) in [
            (
                "fn f() { let x = 5.5.; }",
                "floating-point literal cannot end with `.`: write `5.5` instead of `5.5.`",
            ),
            (
                "fn f() { let x = 1e9.; }",
                "floating-point literal cannot end with `.`: write `1e9` instead of `1e9.`",
            ),
        ] {
            assert_eq!(
                parse_errors(source),
                vec![expected],
                "unexpected diagnostics for {source}"
            );
        }
    }

    #[test]
    fn trailing_dot_rejection_does_not_disturb_member_access() {
        // A member name after the dot is an ordinary access/method call, on an
        // integer literal as much as on anything else.
        assert!(parses("fn f() { 42.to_string() }"));
        assert!(parses("fn f() { 5.5.to_string() }"));
        assert!(parses("fn f(p: P) { p.x }"));
        // A space before the dot is not the trailing-dot spelling; it stays an
        // ordinary "expected identifier" diagnostic.
        let errors = parse_errors("fn f() { let x = 5 .; }");
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("expected identifier"),
            "unexpected diagnostic: {errors:?}"
        );
    }

    #[test]
    fn trailing_dot_float_recovers_to_the_next_item() {
        // One diagnostic, not a cascade: recovery resumes at the next item, so
        // the following function still parses.
        let errors = parse_errors("fn f() -> i32 { let x = 5.; 1 } fn g() -> i32 { 2 }");
        assert_eq!(
            errors,
            vec!["floating-point literal cannot end with `.`: write `5.0` instead of `5.`"]
        );

        let errors = parse_errors("fn f() -> i32 { let x = 5.5.; 1 } fn g() -> i32 { 2 }");
        assert_eq!(
            errors,
            vec!["floating-point literal cannot end with `.`: write `5.5` instead of `5.5.`"]
        );
    }
}
