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
        Ok(Expr::If(IfExpr {
            cond: Box::new(cond),
            then_block,
            else_block,
            span: self.span_from(start),
        }))
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
        // Exactly one identifier — the variant — may follow constructor
        // arguments. Reject a group attached to an earlier module segment
        // instead of silently moving it onto the final type-name segment.
        let mut segments_after_ctor = ctor_args.as_ref().map(|_| 1usize);
        if ctor_args.is_none() && self.at(TokenKind::LParen) && self.paren_group_precedes_dot() {
            ctor_args = Some(self.call_args()?);
            segments_after_ctor = Some(0);
        }
        while self.eat(TokenKind::Dot) {
            segments.push(self.ident()?);
            if let Some(count) = &mut segments_after_ctor {
                *count += 1;
            }
            if ctor_args.is_none() && self.at(TokenKind::LParen) && self.paren_group_precedes_dot()
            {
                ctor_args = Some(self.call_args()?);
                segments_after_ctor = Some(0);
            }
        }
        if matches!(segments_after_ctor, Some(count) if count != 1) {
            self.error(
                "type-constructor arguments in a pattern must follow the final type path segment",
            );
            return Err(());
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

    pub(super) fn block(&mut self) -> PResult<BlockExpr> {
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
                self.pratt_tail(block_like, 0)?
            } else {
                self.expr()?
            };
            // `place = value` and the compound forms `place op= value`
            // (RUE-1043) share one statement shape; the operator, if any, is
            // recorded on the statement and applied by the desugaring in RIR.
            let compound = CompoundOp::from_token(self.kind());
            if compound.is_some() || self.at(TokenKind::Eq) {
                self.bump();
                let target = expr_to_target(value, self.syms.self_value).ok_or_else(|| {
                    self.error("invalid assignment target");
                })?;
                let rhs = Box::new(self.expr()?);
                self.expect(TokenKind::Semi)?;
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
}
