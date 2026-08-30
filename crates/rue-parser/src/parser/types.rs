//! Type expressions, type-position constructors, and inline type members.

use super::*;

impl Parser {
    pub(super) fn ty(&mut self) -> PResult<TypeExpr> {
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
            // An anonymous struct or enum declaration expression is producer-
            // nominal (ADR-0066, rule 4.14:23a) and may not appear directly or
            // nested within a type annotation — `let`, parameter, return, field,
            // array-element, pointer-pointee, and type-constructor-argument
            // positions all reach `ty()`. They remain legal as comptime values
            // and as type-constructor results, so the fix is to bind the type or
            // name it through a constructor. The keyword token is the accurate
            // span; `self.error` reports at the current (unbumped) token.
            TokenKind::Struct => {
                self.error(
                    "an anonymous `struct` type cannot appear in a type annotation; \
                     bind it with `let` or return it from a type constructor and use \
                     that name here",
                );
                Err(())
            }
            TokenKind::Enum => {
                self.error(
                    "an anonymous `enum` type cannot appear in a type annotation; \
                     bind it with `let` or return it from a type constructor and use \
                     that name here",
                );
                Err(())
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

    pub(super) fn primitive_spur(&self, kind: TokenKind) -> Option<Spur> {
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

    pub(super) fn anonymous_struct_type(&mut self, allow_methods: bool) -> PResult<TypeExpr> {
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

    pub(super) fn method(&mut self) -> PResult<Method> {
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
        let (return_type, place_return) = self.return_type_with_place_mode()?;
        let anonymous_mark = self.anonymous_literal_mark();
        let body = Expr::Block(self.block()?);
        let contains_anonymous_type_literal = self.saw_anonymous_literal_since(anonymous_mark);
        Ok(Method {
            contains_anonymous_type_literal,
            directives,
            name,
            receiver,
            params,
            return_type,
            place_return,
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
        let anonymous_mark = self.anonymous_literal_mark();
        let body = Expr::Block(self.block()?);
        let contains_anonymous_type_literal = self.saw_anonymous_literal_since(anonymous_mark);
        let span = self.span_from(start);
        Ok(Method {
            contains_anonymous_type_literal,
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
            place_return: None,
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
    fn parses_nested_type_position_forms() {
        assert!(parses(
            "fn f(x: ptr const [Result(i32, [u8]); N]) -> Out { loop {} }"
        ));
    }

    #[test]
    fn rejects_an_anonymous_struct_in_return_position() {
        // Anonymous type literals are creation sites, legal only in comptime
        // expression position; the return annotation is type grammar (RUE-1089).
        assert!(!parses(
            "fn f(x: ptr const [Result(i32, [u8]); N]) -> struct { value: i32 } { loop {} }"
        ));
    }

    #[test]
    fn rejects_an_incomplete_pointer_type() {
        assert!(!parses("fn f(x: ptr i32) {}"));
    }
}
