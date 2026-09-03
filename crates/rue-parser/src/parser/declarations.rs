//! Directives, top-level items, declarations, fields, and parameters.

use super::*;

impl Parser {
    pub(super) fn directives(&mut self) -> PResult<Directives> {
        let mut out = Directives::new();
        while self.at(TokenKind::At) {
            let start = self.bump().span.start;
            let name = self.ident()?;
            let mut args = Vec::new();
            if self.eat(TokenKind::LParen) {
                args = self.comma_separated(TokenKind::RParen, |parser| {
                    parser.ident().map(DirectiveArg::Ident)
                })?;
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

    pub(super) fn item(&mut self) -> PResult<Item> {
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
            TokenKind::Enum => self
                .enum_decl(start, directives, visibility)
                .map(Item::Enum),
            TokenKind::Drop if directives.is_empty() && visibility == Visibility::Private => {
                self.drop_fn(start).map(Item::DropFn)
            }
            // `pub extern "C" fn name(...) { body }` is a Rue-to-C *export*
            // (ADR-0064 P4): an ordinary Rue function body also exposed to C
            // callers under its unmangled name. It is distinguished from the
            // private import block below purely by its `pub` visibility and its
            // trailing `fn` (the block form has no `fn` after the ABI string).
            TokenKind::Extern if directives.is_empty() && visibility == Visibility::Public => self
                .extern_export_fn(start, directives, visibility)
                .map(Item::Function),
            TokenKind::Extern if directives.is_empty() && visibility == Visibility::Private => {
                self.extern_block(start).map(Item::Extern)
            }
            TokenKind::Const => self
                .const_decl(start, directives, visibility)
                .map(Item::Const),
            // `test "name" { .. }` (ADR-0083 §1). `test` is Rue's first
            // contextual keyword: it is recognized here only when it is an
            // identifier spelled `test` followed by a string literal, so a
            // `fn test`, a `const test`, and a local `let test = ..` all keep
            // their ordinary meaning. A `pub`/`unchecked` prefix is not part of
            // the production, so `pub test "x" {}` falls through to the
            // unexpected-token arm below.
            _ if visibility == Visibility::Private && self.at_test_item() => {
                self.test_decl(start, directives).map(Item::Test)
            }
            _ => {
                self.unexpected("'@' or 'pub' or 'unchecked' or 'fn' or 'test' or …");
                Err(())
            }
        }
    }

    /// Whether the cursor is at the contextual `test` keyword introducing a
    /// test declaration: an identifier spelled `test` immediately followed by a
    /// string literal (ADR-0083 §1). This is the single authority on the
    /// contextual-keyword decision; item recovery consults it too.
    pub(super) fn at_test_item(&self) -> bool {
        matches!(self.kind(), TokenKind::Ident(spur) if spur == self.syms.test_kw)
            && matches!(self.nth(1), TokenKind::String(_))
    }

    /// Parse `test "name" { .. }`. The caller has already parsed directives and
    /// established that [`Parser::at_test_item`] holds.
    fn test_decl(&mut self, start: u32, directives: Directives) -> PResult<TestDecl> {
        self.bump(); // the contextual `test` keyword
        let name = match self.kind() {
            TokenKind::String(value) => {
                let span = self.bump().span;
                StringLit { value, span }
            }
            _ => {
                self.unexpected("a test name string literal");
                return Err(());
            }
        };
        let header_span = self.span_from(start);
        let anonymous_mark = self.anonymous_literal_mark();
        let body = Expr::Block(self.block()?);
        let contains_anonymous_type_literal = self.saw_anonymous_literal_since(anonymous_mark);
        Ok(TestDecl {
            directives,
            name,
            body,
            span: self.span_from(start),
            header_span,
            contains_anonymous_type_literal,
        })
    }

    fn function(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<Function> {
        self.function_inner(start, directives, visibility, None)
    }

    /// Parse a `pub extern "C" fn name(...) { body }` Rue-to-C export
    /// (ADR-0064 P4). The `extern` token is consumed here, then the ABI string
    /// is captured verbatim and validated (`"C"` only) exactly as the import
    /// block does, and the rest is an ordinary function with a body. The parsed
    /// ABI is attached to the `Function` so semantic analysis can gate it behind
    /// the `c_ffi` preview and validate the C-boundary signature.
    fn extern_export_fn(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<Function> {
        self.expect(TokenKind::Extern)?;
        let (abi, abi_span) = match self.kind() {
            TokenKind::String(spur) => {
                let span = self.bump().span;
                (self.interner.resolve(&spur).to_string(), span)
            }
            _ => {
                self.unexpected("an ABI string such as \"C\"");
                return Err(());
            }
        };
        if abi != "C" {
            self.error_at(
                format!(
                    "unsupported extern ABI \"{abi}\": only \"C\" is supported \
                     (ADR-0064 C FFI)"
                ),
                abi_span,
            );
        }
        self.function_inner(start, directives, visibility, Some(abi))
    }

    fn function_inner(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
        export_abi: Option<String>,
    ) -> PResult<Function> {
        let is_unchecked = self.eat(TokenKind::Unchecked);
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        let params = self.params()?;
        let (return_type, place_return) = self.return_type_with_place_mode()?;
        let anonymous_mark = self.anonymous_literal_mark();
        let body = Expr::Block(self.block()?);
        let contains_anonymous_type_literal = self.saw_anonymous_literal_since(anonymous_mark);
        Ok(Function {
            contains_anonymous_type_literal,
            directives,
            visibility,
            is_unchecked,
            name,
            params,
            return_type,
            place_return,
            body,
            export_abi,
            span: self.span_from(start),
        })
    }

    /// Parse an optional `-> [borrow|inout] type` result position. The
    /// qualifier and its span are retained so semantic analysis can gate and
    /// diagnose place-returning accessors (ADR-0062).
    pub(super) fn return_type_with_place_mode(
        &mut self,
    ) -> PResult<(Option<TypeExpr>, Option<PlaceReturn>)> {
        if !self.eat(TokenKind::Arrow) {
            return Ok((None, None));
        }
        let place_return = if self.at(TokenKind::Borrow) {
            Some(PlaceReturn::Borrow(self.bump().span))
        } else if self.at(TokenKind::Inout) {
            Some(PlaceReturn::Inout(self.bump().span))
        } else {
            None
        };
        Ok((Some(self.ty()?), place_return))
    }

    /// Parse a foreign-declaration block: `extern "C" { fn name(...) -> T; }`.
    ///
    /// The ABI string is captured verbatim (validated in semantic analysis so
    /// the diagnostic can name the unsupported ABI). Each member is a body-less
    /// function signature terminated by `;`.
    fn extern_block(&mut self, start: u32) -> PResult<ExternBlock> {
        self.expect(TokenKind::Extern)?;
        let (abi, abi_span) = match self.kind() {
            TokenKind::String(spur) => {
                let span = self.bump().span;
                (self.interner.resolve(&spur).to_string(), span)
            }
            _ => {
                self.unexpected("an ABI string such as \"C\"");
                return Err(());
            }
        };
        // `"C"` is the only ABI the current C FFI phase accepts (ADR-0064). The
        // slot reserves room for later `"C-unwind"` or platform variants.
        if abi != "C" {
            self.error_at(
                format!(
                    "unsupported extern ABI \"{abi}\": only \"C\" is supported \
                     (ADR-0064 C FFI)"
                ),
                abi_span,
            );
        }
        self.expect(TokenKind::LBrace)?;
        let mut fns = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            fns.push(self.extern_fn()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(ExternBlock {
            abi,
            abi_span,
            fns,
            span: self.span_from(start),
        })
    }

    /// Parse a single body-less foreign function signature inside an `extern`
    /// block: `fn name(params) -> ret;`.
    fn extern_fn(&mut self) -> PResult<ExternFn> {
        let start = self.start();
        self.expect(TokenKind::Fn)?;
        let name = self.ident()?;
        let params = self.extern_params()?;
        let return_type = if self.eat(TokenKind::Arrow) {
            Some(self.ty()?)
        } else {
            None
        };
        self.expect(TokenKind::Semi)?;
        Ok(ExternFn {
            name,
            params,
            return_type,
            span: self.span_from(start),
        })
    }

    /// Parse a foreign function's parameter list, recognizing the C variadic
    /// marker `...` where a parameter is expected.
    ///
    /// Ordinary parameter lists (`self.params()`) reach `...` only as a stray
    /// `.` and report a generic "unexpected token". Variadics are a real C
    /// surface a reader will reach for, so the extern boundary detects the
    /// marker specifically and routes it to the dedicated
    /// [`ErrorKind::ExternVariadicUnsupported`] diagnostic (ADR-0064 secondary
    /// ruling B, P6): variadic foreign calls are rejected in v0. The marker is
    /// valid only after zero or more fixed parameters, so it is checked at each
    /// position a parameter would begin — covering both `fn f(...)` and
    /// `fn f(a: i32, ...)`.
    fn extern_params(&mut self) -> PResult<Vec<Param>> {
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Dot) {
                return self.reject_variadic_ellipsis();
            }
            params.push(self.param()?);
            if !self.eat(TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(params)
    }

    /// Record the variadic-rejection diagnostic spanning the `...` marker and
    /// abort the current declaration. `...` lexes as three consecutive `.`
    /// tokens; the span covers the whole marker so the caret underlines it.
    fn reject_variadic_ellipsis(&mut self) -> PResult<Vec<Param>> {
        let start = self.start();
        let mut end = start;
        while self.at(TokenKind::Dot) {
            end = self.bump().span.end;
        }
        self.record_error(CompileError::new(
            ErrorKind::ExternVariadicUnsupported,
            Span::with_file(self.file_id, start, end),
        ));
        Err(())
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

    fn enum_decl(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<EnumDecl> {
        self.expect(TokenKind::Enum)?;
        let name = self.ident()?;
        let variants = self.enum_variants()?;
        Ok(EnumDecl {
            directives,
            visibility,
            name,
            variants,
            span: self.span_from(start),
        })
    }

    pub(super) fn enum_variants(&mut self) -> PResult<Vec<EnumVariant>> {
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
        let anonymous_mark = self.anonymous_literal_mark();
        let body = Expr::Block(self.block()?);
        let contains_anonymous_type_literal = self.saw_anonymous_literal_since(anonymous_mark);
        Ok(DropFn {
            contains_anonymous_type_literal,
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
        let anonymous_mark = self.anonymous_literal_mark();
        let init = Box::new(self.expr()?);
        let contains_anonymous_type_literal = self.saw_anonymous_literal_since(anonymous_mark);
        self.expect(TokenKind::Semi)?;
        Ok(ConstDecl {
            contains_anonymous_type_literal,
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
        let out = self.comma_separated(TokenKind::RParen, Self::param)?;
        self.expect(TokenKind::RParen)?;
        Ok(out)
    }

    pub(super) fn param(&mut self) -> PResult<Param> {
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
    fn parses_item_and_declaration_forms() {
        assert!(parses(
            "pub fn id(x: i32) -> i32 { x } struct Pair { left: i32, right: i32 } enum Choice { One, Two(i32) } const N: i32 = 1;"
        ));
    }

    #[test]
    fn rejects_a_declaration_without_a_name() {
        assert!(!parses("fn () -> i32 { 0 }"));
    }
}
