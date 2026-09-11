//! Blocks, statements, patterns, and control-flow expressions.

use super::expressions::binary_binding;
use super::*;

impl Parser {
    pub(super) fn if_expr(&mut self) -> PResult<Expr> {
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
        Ok(Expr::If(Box::new(IfExpr {
            cond: Box::new(cond),
            then_block,
            else_block,
            span: self.span_from(start),
        })))
    }
    pub(super) fn while_expr(&mut self) -> PResult<Expr> {
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
    pub(super) fn for_expr(&mut self) -> PResult<Expr> {
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
    pub(super) fn loop_expr(&mut self) -> PResult<Expr> {
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

    pub(super) fn match_expr(&mut self) -> PResult<Expr> {
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

    /// One enum-variant pattern: `path_pattern = pattern_head "." IDENT
    /// [ "(" pattern_elements ")" ]` (spec 4.7:2).
    ///
    /// The head is parsed by [`Parser::path_head`], the grammar's one
    /// path-head parser, so a pattern head is built from the same forms and
    /// carried in the same shape as an expression's struct-literal head. What is left here is
    /// the classification the pattern grammar adds on top: the trailing name is
    /// the variant, and a terminal `(...)` is its payload list of patterns
    /// rather than the call arguments an expression would parse there.
    fn path_pattern(&mut self, start: u32) -> PResult<Pattern> {
        let (head, variant) = self.path_head()?;
        let mut elements = Vec::new();
        if self.eat(TokenKind::LParen) {
            if self.at(TokenKind::RParen) {
                self.error("expected payload binding");
                return Err(());
            }
            loop {
                elements.push(self.pattern_element()?);
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
            base: head.base,
            type_name: head.name,
            ctor_args: head.ctor_args,
            variant,
            elements,
            span: self.span_from(start),
        }))
    }

    /// One payload position of a tuple-variant pattern: a binder, the `_`
    /// discard, or a nested variant pattern (RUE-2053). A nested pattern is
    /// parsed by the same `path_pattern` this method is called from — the
    /// pattern grammar has exactly one path parser (RUE-1988), so a payload
    /// position accepts every head form a top-level pattern accepts
    /// (`E.A(b)`, `m.E.A(b)`, `Result(i32, E).Ok(v)`).
    fn pattern_element(&mut self) -> PResult<PatternElement> {
        if self.at(TokenKind::Underscore) {
            let span = self.bump().span;
            return Ok(PatternElement::Binding(Ident {
                name: self.syms.underscore,
                span,
            }));
        }
        // A binder is a lone identifier, so the position is a nested pattern
        // exactly when the identifier continues into a path (`E.A`) or a
        // type-constructor head (`Result(i32, E).Ok`).
        if matches!(self.kind(), TokenKind::Ident(_))
            && matches!(self.nth(1), TokenKind::Dot | TokenKind::LParen)
        {
            let start = self.start();
            let Pattern::Path(nested) = self.path_pattern(start)? else {
                unreachable!("path_pattern yields a path pattern")
            };
            return Ok(PatternElement::Nested(nested));
        }
        Ok(PatternElement::Binding(self.ident()?))
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

    pub(super) fn block(&mut self) -> PResult<BlockExpr> {
        let start = self.start();
        self.expect(TokenKind::LBrace)?;
        let mut statements = Vec::new();
        let mut final_expr = None;
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let directed_let = self.at(TokenKind::At) && self.directive_is_followed_by_let();
            if self.at(TokenKind::Let) || (self.at(TokenKind::At) && directed_let) {
                let statement_start = self.cursor;
                match self.let_statement() {
                    Ok(statement) => statements.push(statement),
                    Err(()) => {
                        let span = self.recover_statement(statement_start);
                        statements.push(Statement::Expr(Expr::Error(span)));
                    }
                }
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
            let value = match (|| -> PResult<Expr> {
                if matches!(
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
                            return Ok(block_like);
                        }
                        if binary_binding(self.kind()).is_some() {
                            let op_span = self
                                .tokens
                                .get(self.cursor)
                                .map(|token| token.span)
                                .unwrap_or_else(|| {
                                    Span::point_in_file(self.file_id, self.end_offset())
                                });
                            self.record_error(
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
                    self.pratt_tail(block_like, 0)
                } else {
                    self.expr()
                }
            })() {
                Ok(value) => value,
                Err(()) => {
                    let span = self.recover_statement(expr_start);
                    statements.push(Statement::Expr(Expr::Error(span)));
                    continue;
                }
            };
            // `place = value` and the compound forms `place op= value`
            // (RUE-1043) share one statement shape; the operator, if any, is
            // recorded on the statement and applied by the desugaring in RIR.
            let compound = CompoundOp::from_token(self.kind());
            if compound.is_some() || self.at(TokenKind::Eq) {
                self.bump();
                let Some(target) = expr_to_target(value, self.syms.self_value) else {
                    self.error("invalid assignment target");
                    let span = self.recover_statement(expr_start);
                    statements.push(Statement::Expr(Expr::Error(span)));
                    continue;
                };
                let rhs = match self.expr() {
                    Ok(rhs) => Box::new(rhs),
                    Err(()) => {
                        let span = self.recover_statement(expr_start);
                        statements.push(Statement::Expr(Expr::Error(span)));
                        continue;
                    }
                };
                if !self.eat(TokenKind::Semi) {
                    self.unexpected("';'");
                    let span = self.recover_statement(expr_start);
                    statements.push(Statement::Expr(Expr::Error(span)));
                    continue;
                }
                statements.push(Statement::Assign(AssignStatement {
                    target,
                    op: compound,
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
            let span = self.recover_statement(expr_start);
            statements.push(Statement::Expr(Expr::Error(span)));
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
            // Empty stays `None`: allocating for the common undirected `let`
            // would trade the inline bytes for a heap trip (RUE-1836).
            directives: (!directives.is_empty()).then(|| Box::new(directives)),
            is_mut,
            pattern,
            ty: ty.map(Box::new),
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
}

fn expr_to_target(expr: Expr, self_value: Spur) -> Option<AssignTarget> {
    if contains_method_call(&expr) {
        return Some(AssignTarget::Method(Box::new(expr)));
    }
    match expr {
        Expr::Ident(id) => Some(AssignTarget::Var(id)),
        Expr::Field(field) => Some(AssignTarget::Field(field)),
        Expr::Index(index) => Some(AssignTarget::Index(index)),
        Expr::MethodCall(call) => Some(AssignTarget::Method(Box::new(Expr::MethodCall(call)))),
        // `self = value` targets the receiver binding; sema enforces that
        // only a `mut self` (or shadowing `let mut`) receiver is assignable.
        Expr::SelfExpr(se) => Some(AssignTarget::Var(Ident {
            name: self_value,
            span: se.span,
        })),
        _ => None,
    }
}

fn contains_method_call(expr: &Expr) -> bool {
    match expr {
        Expr::MethodCall(_) => true,
        Expr::Field(field) => contains_method_call(&field.base),
        Expr::Index(index) => contains_method_call(&index.base),
        Expr::Paren(paren) => contains_method_call(&paren.inner),
        _ => false,
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
            | Expr::Yield(_)
            | Expr::Block(_)
    )
}
fn is_diverging(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Break(_) | Expr::Continue(_) | Expr::Return(_) | Expr::Yield(_) | Expr::Loop(_)
    )
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
    fn parses_statements_and_control_flow() {
        assert!(parses(
            "fn f(x: i32) -> i32 { let mut y: i32 = x; while y > 0 { y = y - 1; } if y == 0 { 1 } else { 2 } }"
        ));
    }

    fn assignment_of(source: &str) -> AssignStatement {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, _) = Parser::new(tokens, interner).parse().unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected a function item");
        };
        let Expr::Block(body) = &function.body else {
            panic!("expected a block body");
        };
        match &body.statements[1] {
            Statement::Assign(assignment) => assignment.clone(),
            other => panic!("expected an assignment statement, got {other:?}"),
        }
    }

    #[test]
    fn compound_assignment_records_its_operator() {
        for (source, expected) in [
            ("x += 1;", CompoundOp::Add),
            ("x -= 1;", CompoundOp::Sub),
            ("x *= 1;", CompoundOp::Mul),
            ("x /= 1;", CompoundOp::Div),
            ("x %= 1;", CompoundOp::Mod),
            ("x &= 1;", CompoundOp::BitAnd),
            ("x |= 1;", CompoundOp::BitOr),
            ("x ^= 1;", CompoundOp::BitXor),
            ("x <<= 1;", CompoundOp::Shl),
            ("x >>= 1;", CompoundOp::Shr),
        ] {
            let assignment = assignment_of(&format!("fn f() {{ let mut x = 0; {source} }}"));
            assert_eq!(assignment.op, Some(expected), "for `{source}`");
            assert!(matches!(assignment.target, AssignTarget::Var(_)));
        }
    }

    #[test]
    fn plain_assignment_records_no_operator() {
        let assignment = assignment_of("fn f() { let mut x = 0; x = 1; }");
        assert_eq!(assignment.op, None);
    }

    #[test]
    fn compound_assignment_accepts_every_place_form() {
        assert!(parses("fn f() { let mut x = 0; x += 1; }"));
        assert!(parses("fn f(inout p: P) { p.field += 1; }"));
        assert!(parses("fn f(inout a: [i32; 2]) { a[0] += 1; }"));
        assert!(parses("fn f(inout o: O) { o.rows[1].cells[i] *= 2; }"));
    }

    #[test]
    fn compound_assignment_is_a_statement_not_an_expression() {
        assert!(!parses("fn f() { let mut x = 0; let y = (x += 1); y }"));
    }

    #[test]
    fn rejects_a_let_statement_without_a_terminator() {
        assert!(!parses("fn f() { let x = 1 x }"));
    }

    fn parse_errors(source: &str) -> Vec<String> {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        match Parser::new(tokens, interner).parse() {
            Ok(_) => Vec::new(),
            Err(errors) => errors.iter().map(|error| error.to_string()).collect(),
        }
    }

    /// The pattern of the first arm of the `match` that is `f`'s body.
    fn first_arm_pattern(source: &str) -> PathPattern {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, _) = Parser::new(tokens, interner).parse().unwrap();
        let Item::Function(function) = &ast.items[0] else {
            panic!("expected a function item");
        };
        let Expr::Block(body) = &function.body else {
            panic!("expected a block body");
        };
        let Expr::Match(match_expr) = &*body.expr else {
            panic!("expected a match expression, got {:?}", body.expr);
        };
        match &match_expr.arms[0].pattern {
            Pattern::Path(path) => path.clone(),
            other => panic!("expected a path pattern, got {other:?}"),
        }
    }

    /// The `.`-separated identifiers a pattern head's module base is spelled
    /// as, innermost first, resolved through the interner.
    fn base_segments(pattern: &PathPattern, interner: &lasso::ThreadedRodeo) -> Vec<String> {
        let mut segments = Vec::new();
        let mut cursor = pattern.base.as_deref();
        while let Some(expr) = cursor {
            match expr {
                Expr::Field(field) => {
                    segments.push(interner.resolve(&field.field.name).to_owned());
                    cursor = Some(&field.base);
                }
                Expr::Ident(ident) => {
                    segments.push(interner.resolve(&ident.name).to_owned());
                    cursor = None;
                }
                other => panic!("a pattern head base is a field chain, got {other:?}"),
            }
        }
        segments.reverse();
        segments
    }

    fn interner_of(source: &str) -> lasso::ThreadedRodeo {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner).parse().unwrap().1
    }

    #[test]
    fn pattern_constructor_arguments_follow_the_final_type_segment() {
        assert!(parses(
            "fn f(x: i32) -> i32 { match x { Result(i32, i32).Ok(v) => v } }"
        ));
        assert!(parses(
            "fn f(x: i32) -> i32 { match x { std.result.Result(i32, i32).Ok(v) => v } }"
        ));
        assert!(!parses(
            "fn f(x: i32) -> i32 { match x { std(i32).result.Result.Ok(v) => v } }"
        ));
    }

    /// A module-qualified head carries its module segments as the same field
    /// chain an expression path builds, with and without payload bindings.
    #[test]
    fn module_qualified_variant_pattern_carries_a_field_chain_base() {
        for (source, bindings) in [
            ("fn f(x: i32) -> i32 { match x { a.b.E.A => 0 } }", 0),
            ("fn f(x: i32) -> i32 { match x { a.b.E.A(v) => v } }", 1),
        ] {
            let interner = interner_of(source);
            let pattern = first_arm_pattern(source);
            assert_eq!(base_segments(&pattern, &interner), ["a", "b"], "{source}");
            assert_eq!(interner.resolve(&pattern.type_name.name), "E", "{source}");
            assert_eq!(interner.resolve(&pattern.variant.name), "A", "{source}");
            assert!(pattern.ctor_args.is_none(), "{source}");
            assert_eq!(pattern.elements.len(), bindings, "{source}");
        }
    }

    /// A generic-call head attaches its arguments to the final type segment,
    /// leaving the module segments before it as the base.
    #[test]
    fn generic_call_head_attaches_to_the_final_type_segment() {
        let source = "fn f(x: i32) -> i32 { match x { m.Generic(i32, u8).Variant(v) => v } }";
        let interner = interner_of(source);
        let pattern = first_arm_pattern(source);
        assert_eq!(base_segments(&pattern, &interner), ["m"]);
        assert_eq!(interner.resolve(&pattern.type_name.name), "Generic");
        assert_eq!(interner.resolve(&pattern.variant.name), "Variant");
        assert_eq!(pattern.ctor_args.as_ref().map(Vec::len), Some(2));
        assert_eq!(pattern.elements.len(), 1);
    }

    /// A payload position parses with the same head parser, so every head form
    /// nests (RUE-2053).
    #[test]
    fn nested_variant_patterns_accept_every_head_form() {
        for source in [
            "fn f(x: i32) -> i32 { match x { E.A(F.B) => 0 } }",
            "fn f(x: i32) -> i32 { match x { E.A(F.B(v)) => v } }",
            "fn f(x: i32) -> i32 { match x { E.A(m.F.B(v), _) => v } }",
            "fn f(x: i32) -> i32 { match x { E.A(Result(i32, u8).Ok(v)) => v } }",
            "fn f(x: i32) -> i32 { match x { E.A(F.B(G.C(v))) => v } }",
        ] {
            assert!(parses(source), "{source}");
        }
        let source = "fn f(x: i32) -> i32 { match x { E.A(m.F.B(v), _) => v } }";
        let interner = interner_of(source);
        let pattern = first_arm_pattern(source);
        let PatternElement::Nested(nested) = &pattern.elements[0] else {
            panic!("expected a nested pattern");
        };
        assert_eq!(base_segments(nested, &interner), ["m"]);
        assert_eq!(interner.resolve(&nested.type_name.name), "F");
        assert_eq!(interner.resolve(&nested.variant.name), "B");
        assert!(matches!(pattern.elements[1], PatternElement::Binding(_)));
    }

    /// The pattern grammar is narrower than the expression path grammar: a
    /// parenthesized head or `Self` reaches sema in expression position but is
    /// not a `pattern_head` (spec 4.7:2), and the head parser does not widen it.
    #[test]
    fn non_pattern_heads_are_still_rejected_as_patterns() {
        for source in [
            "fn f(x: i32) -> i32 { match x { (Color).Red => 0 } }",
            "fn f(x: i32) -> i32 { match x { Self.Red => 0 } }",
            "fn f(x: i32) -> i32 { match x { (Color.Red) => 0 } }",
        ] {
            assert_eq!(
                parse_errors(source).first().map(String::as_str),
                Some("expected pattern"),
                "{source}"
            );
        }
    }

    /// A head with no variant reports the missing `.` at the token that ends
    /// the head, not at the group the head consumed on the way there.
    #[test]
    fn a_head_without_a_variant_reports_the_missing_dot() {
        assert_eq!(
            parse_errors("fn f(x: i32) -> i32 { match x { Some(v) => v } }").first(),
            Some(&"expected '.', found '=>'".to_owned())
        );
        assert_eq!(
            parse_errors("fn f(x: i32) -> i32 { match x { Color => 0 } }").first(),
            Some(&"expected '.', found '=>'".to_owned())
        );
    }
}
