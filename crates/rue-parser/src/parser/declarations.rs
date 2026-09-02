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
            TokenKind::Interface => self
                .interface_decl(start, directives, visibility)
                .map(Item::Interface),
            TokenKind::Drop if directives.is_empty() && visibility == Visibility::Private => {
                self.drop_fn(start).map(Item::DropFn)
            }
            // A freestanding conformance assertion `Type is Interface;` (spec
            // 6.7:9) is the only item that begins with a type rather than a
            // keyword. It takes neither directives nor a visibility modifier.
            kind if directives.is_empty()
                && visibility == Visibility::Private
                && self.starts_conformance_subject(kind) =>
            {
                self.conformance_decl(start).map(Item::Conformance)
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
        let conformances = if self.at_is_keyword() {
            self.bump();
            self.interface_list()?
        } else {
            Vec::new()
        };
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        let mut assoc_types = Vec::new();
        let mut methods = Vec::new();
        let mut saw_method = false;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            if self.at(TokenKind::Fn) || self.at(TokenKind::At) {
                saw_method = true;
                methods.push(self.method()?);
            } else if self.at(TokenKind::Pub) || self.at(TokenKind::Const) {
                if saw_method {
                    self.error("struct associated types must precede methods");
                    return Err(());
                }
                assoc_types.push(self.assoc_type_decl()?);
            } else {
                if saw_method {
                    self.error("struct fields must precede methods");
                    return Err(());
                }
                if !assoc_types.is_empty() {
                    self.error("struct fields must precede associated types");
                    return Err(());
                }
                fields.push(self.field_decl()?);
                if !self.eat(TokenKind::Comma)
                    && !self.at(TokenKind::RBrace)
                    && !self.at(TokenKind::Fn)
                    && !self.at(TokenKind::At)
                    && !self.at(TokenKind::Pub)
                    && !self.at(TokenKind::Const)
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
            conformances,
            fields,
            assoc_types,
            methods,
            span: self.span_from(start),
        })
    }

    /// Parse a struct-body associated type declaration
    /// `[pub] const Name = Type;` (spec 6.7:2, `struct_assoc_type`).
    fn assoc_type_decl(&mut self) -> PResult<AssocTypeDecl> {
        let start = self.start();
        let visibility = if self.eat(TokenKind::Pub) {
            Visibility::Public
        } else {
            Visibility::Private
        };
        self.expect(TokenKind::Const)?;
        let name = self.ident()?;
        self.expect(TokenKind::Eq)?;
        let ty = self.ty()?;
        self.expect(TokenKind::Semi)?;
        Ok(AssocTypeDecl {
            visibility,
            name,
            ty,
            span: self.span_from(start),
        })
    }

    /// Whether the current token is the contextual keyword `is` (spec 6.7:9).
    fn at_is_keyword(&self) -> bool {
        matches!(self.kind(), TokenKind::Ident(name) if name == self.syms.is_kw)
    }

    /// Whether `kind` can begin the subject type of a freestanding
    /// conformance assertion. Every other item begins with a keyword, `@`, or
    /// `pub`, so this set never overlaps the keyword dispatch above.
    fn starts_conformance_subject(&self, kind: TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Ident(_) | TokenKind::LBracket | TokenKind::Ptr | TokenKind::SelfType
        ) || self.primitive_spur(kind).is_some()
    }

    /// Parse `Type is Interface + Other;` (spec 6.7:2, `conformance_decl`).
    fn conformance_decl(&mut self, start: u32) -> PResult<ConformanceDecl> {
        let subject = self.ty()?;
        if !self.at_is_keyword() {
            self.unexpected("'is'");
            return Err(());
        }
        self.bump();
        let interfaces = self.interface_list()?;
        self.expect(TokenKind::Semi)?;
        Ok(ConformanceDecl {
            subject,
            interfaces,
            span: self.span_from(start),
        })
    }

    /// Parse a non-empty `+`-separated interface list (spec 6.7:2,
    /// `interface_list`). Each element is a type expression so module-qualified
    /// interface names resolve through the ordinary type path.
    pub(super) fn interface_list(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut interfaces = vec![self.ty()?];
        while self.eat(TokenKind::Plus) {
            interfaces.push(self.ty()?);
        }
        Ok(interfaces)
    }

    /// Parse `[pub] interface Name [: Parent + Other] { requirements }`
    /// (spec 6.7:2, `interface_def`).
    fn interface_decl(
        &mut self,
        start: u32,
        directives: Directives,
        visibility: Visibility,
    ) -> PResult<InterfaceDecl> {
        self.expect(TokenKind::Interface)?;
        let name = self.ident()?;
        let parents = if self.eat(TokenKind::Colon) {
            self.interface_list()?
        } else {
            Vec::new()
        };
        self.expect(TokenKind::LBrace)?;
        let mut requirements = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            requirements.push(self.interface_requirement()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(InterfaceDecl {
            directives,
            visibility,
            name,
            parents,
            requirements,
            span: self.span_from(start),
        })
    }

    /// Parse one interface member: `const Name: type;` or a bodiless method
    /// signature `fn name(...) [-> result];` (spec 6.7:2, `interface_member`).
    fn interface_requirement(&mut self) -> PResult<InterfaceRequirement> {
        let start = self.start();
        match self.kind() {
            TokenKind::Const => {
                self.bump();
                let name = self.ident()?;
                self.expect(TokenKind::Colon)?;
                self.expect(TokenKind::Type)?;
                self.expect(TokenKind::Semi)?;
                Ok(InterfaceRequirement::AssocType(AssocTypeRequirement {
                    name,
                    span: self.span_from(start),
                }))
            }
            TokenKind::Fn | TokenKind::At => {
                let head = self.method_head()?;
                if self.at(TokenKind::LBrace) {
                    self.error("an interface requirement has no body; end it with ';'");
                    return Err(());
                }
                self.expect(TokenKind::Semi)?;
                Ok(InterfaceRequirement::Method(MethodSig {
                    directives: head.directives,
                    name: head.name,
                    receiver: head.receiver,
                    params: head.params,
                    return_type: head.return_type,
                    place_return: head.place_return,
                    span: self.span_from(start),
                }))
            }
            _ => {
                self.unexpected("'const' or 'fn' or '}'");
                Err(())
            }
        }
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
        // A composed interface bound `comptime T: A + B` (spec 6.7:14) is the
        // only parameter form with more than one type after the colon.
        let mut bounds = Vec::new();
        if mode == ParamMode::Comptime {
            while self.eat(TokenKind::Plus) {
                bounds.push(self.ty()?);
            }
        }
        Ok(Param {
            mode,
            name,
            ty,
            bounds,
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

    fn parse(source: &str) -> (Ast, ThreadedRodeo) {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner)
            .parse()
            .unwrap_or_else(|errors| panic!("{source:?} must parse: {errors:?}"))
    }

    #[test]
    fn parses_interface_declarations_with_every_requirement_form() {
        let (ast, interner) = parse(
            "pub interface Collection: Sequence + std.Equatable { \
                 const Element: type; \
                 fn len(borrow self) -> u64; \
                 fn next(inout self) -> Option(Element); \
                 fn make(n: i64) -> Self; \
                 fn take(self) -> Self; \
                 fn get(borrow self, i: u64) -> borrow Element; \
             }",
        );
        let Item::Interface(interface) = &ast.items[0] else {
            panic!("expected an interface item");
        };
        assert_eq!(interface.visibility, Visibility::Public);
        assert_eq!(interner.resolve(&interface.name.name), "Collection");
        assert_eq!(interface.parents.len(), 2);
        assert!(matches!(interface.parents[0], TypeExpr::Named(_)));
        assert!(matches!(interface.parents[1], TypeExpr::Qualified { .. }));
        assert_eq!(interface.requirements.len(), 6);
        assert_eq!(interface.assoc_type_requirements().count(), 1);
        let methods: Vec<_> = interface.method_requirements().collect();
        assert_eq!(methods.len(), 5);
        assert_eq!(
            methods[0].receiver.as_ref().unwrap().mode,
            ParamMode::Borrow
        );
        assert_eq!(methods[1].receiver.as_ref().unwrap().mode, ParamMode::Inout);
        assert!(methods[2].receiver.is_none());
        assert_eq!(methods[2].params.len(), 1);
        assert_eq!(
            methods[3].receiver.as_ref().unwrap().mode,
            ParamMode::Normal
        );
        assert!(methods[4].place_return.is_some_and(|mode| mode.is_borrow()));
        assert!(interface.span.end > interface.span.start);
    }

    #[test]
    fn parses_struct_header_conformances_and_associated_types() {
        let (ast, interner) = parse(
            "struct Range is Sequence + Equatable { \
                 cur: i64, end: i64, \
                 pub const Element = i64; \
                 const Hidden = [u8; 4]; \
                 fn next(inout self) -> Option(i64) { Option(i64).None } \
             }",
        );
        let Item::Struct(structure) = &ast.items[0] else {
            panic!("expected a struct item");
        };
        assert_eq!(structure.conformances.len(), 2);
        assert_eq!(structure.fields.len(), 2);
        assert_eq!(structure.assoc_types.len(), 2);
        assert_eq!(structure.assoc_types[0].visibility, Visibility::Public);
        assert_eq!(
            interner.resolve(&structure.assoc_types[0].name.name),
            "Element"
        );
        assert_eq!(structure.assoc_types[1].visibility, Visibility::Private);
        assert!(matches!(
            structure.assoc_types[1].ty,
            TypeExpr::Array { .. }
        ));
        assert_eq!(structure.methods.len(), 1);
        // A struct without a header assertion keeps the fields-only shape.
        let (ast, _) = parse("struct P { x: i32 }");
        let Item::Struct(structure) = &ast.items[0] else {
            panic!("expected a struct item");
        };
        assert!(structure.conformances.is_empty());
        assert!(structure.assoc_types.is_empty());
    }

    #[test]
    fn parses_freestanding_conformance_assertions() {
        let (ast, _) = parse(
            "i64 is Equatable; ArrayBuf(u64) is Collection + Equatable; \
             [u8; 4] is Equatable; ptr const i32 is Equatable; std.Point is m.Display;",
        );
        assert_eq!(ast.items.len(), 5);
        for item in &ast.items {
            assert!(matches!(item, Item::Conformance(_)), "{item:?}");
        }
        let Item::Conformance(second) = &ast.items[1] else {
            unreachable!()
        };
        assert!(matches!(second.subject, TypeExpr::TypeCall { .. }));
        assert_eq!(second.interfaces.len(), 2);
    }

    #[test]
    fn parses_interface_bounds_on_comptime_parameters() {
        let (ast, _) = parse(
            "fn contains(comptime T: Equatable, borrow xs: ArrayBuf(T), borrow x: T) -> bool { false } \
             fn f(comptime T: Equatable + Sequence, x: T) -> T { x }",
        );
        let Item::Function(contains) = &ast.items[0] else {
            panic!("expected a function");
        };
        assert!(contains.params[0].bounds.is_empty());
        assert!(matches!(contains.params[0].ty, TypeExpr::Named(_)));
        let Item::Function(f) = &ast.items[1] else {
            panic!("expected a function");
        };
        assert_eq!(f.params[0].mode, ParamMode::Comptime);
        assert_eq!(f.params[0].bounds.len(), 1);
        assert!(f.params[1].bounds.is_empty());
        // `+` after a non-comptime parameter type is not a bound.
        assert!(!parses("fn f(x: i32 + i64) -> i32 { x }"));
    }

    #[test]
    fn is_remains_an_ordinary_identifier_outside_conformance_positions() {
        let (ast, interner) = parse(
            "fn is(is: i32) -> i32 { let is = is; is } struct S { is: i32, fn is(self) -> i32 { self.is } }",
        );
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected a function");
        };
        assert_eq!(interner.resolve(&function.name.name), "is");
        assert_eq!(interner.resolve(&function.params[0].name.name), "is");
        let Item::Struct(structure) = &ast.items[1] else {
            panic!("expected a struct");
        };
        assert_eq!(interner.resolve(&structure.fields[0].name.name), "is");
        assert!(structure.conformances.is_empty());
    }

    #[test]
    fn rejects_malformed_interface_forms() {
        // A requirement with a body.
        assert!(!parses("interface I { fn f(self) -> i32 { 0 } }"));
        // A requirement missing its terminator.
        assert!(!parses("interface I { fn f(self) -> i32 }"));
        // An associated type requirement must be `: type`.
        assert!(!parses("interface I { const E: i32; }"));
        assert!(!parses("interface I { const E = i64; }"));
        // A conformance assertion needs `is`, at least one interface, and `;`.
        assert!(!parses("i64 Equatable;"));
        assert!(!parses("i64 is;"));
        assert!(!parses("i64 is Equatable"));
        assert!(!parses("pub i64 is Equatable;"));
        // Struct members keep their fields, associated types, methods order.
        assert!(!parses("struct S { pub const E = i64; x: i32 }"));
        assert!(!parses("struct S { fn f(self) {} pub const E = i64; }"));
        // `interface` is reserved.
        assert!(!parses("fn interface() {}"));
        // An empty interface is grammatical but rejected by post-parse
        // validation (spec 6.7:6).
        assert!(!parses("interface Marker { }"));
    }
}
