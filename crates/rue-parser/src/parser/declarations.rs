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
            TokenKind::Enum if directives.is_empty() => {
                self.enum_decl(start, visibility).map(Item::Enum)
            }
            TokenKind::Drop if directives.is_empty() && visibility == Visibility::Private => {
                self.drop_fn(start).map(Item::DropFn)
            }
            TokenKind::Extern if directives.is_empty() && visibility == Visibility::Private => {
                self.extern_block(start).map(Item::Extern)
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
        let params = self.params()?;
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
