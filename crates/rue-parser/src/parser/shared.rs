//! Shared token, delimiter-list, block-scan, diagnostic, and recovery helpers.

use super::*;

impl Parser {
    pub(super) fn kind(&self) -> TokenKind {
        self.tokens
            .get(self.cursor)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }
    pub(super) fn nth(&self, n: usize) -> TokenKind {
        self.tokens
            .get(self.cursor + n)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof)
    }
    pub(super) fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind
    }
    pub(super) fn bump(&mut self) -> Token {
        let token = self.tokens.get(self.cursor).cloned().unwrap_or(Token {
            kind: TokenKind::Eof,
            span: Span::point_in_file(self.file_id, self.end_offset()),
        });
        if token.kind != TokenKind::Eof {
            self.cursor += 1;
        }
        token
    }
    pub(super) fn end_offset(&self) -> u32 {
        self.tokens.last().map(|t| t.span.end).unwrap_or(0)
    }
    pub(super) fn start(&self) -> u32 {
        self.tokens
            .get(self.cursor)
            .map(|t| t.span.start)
            .unwrap_or(self.end_offset())
    }
    pub(super) fn previous_end(&self) -> u32 {
        self.cursor
            .checked_sub(1)
            .and_then(|i| self.tokens.get(i))
            .map(|t| t.span.end)
            .unwrap_or(self.start())
    }
    pub(super) fn span_from(&self, start: u32) -> Span {
        Span::with_file(self.file_id, start, self.previous_end())
    }
    pub(super) fn error(&mut self, message: impl Into<String>) {
        let span = self
            .tokens
            .get(self.cursor)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
        self.record_error(CompileError::new(
            ErrorKind::ParseError(message.into()),
            span,
        ));
    }
    pub(super) fn error_at(&mut self, message: impl Into<String>, span: Span) {
        self.record_error(CompileError::new(
            ErrorKind::ParseError(message.into()),
            span,
        ));
    }
    pub(super) fn record_error(&mut self, error: CompileError) {
        self.errors.push(error);
    }
    pub(super) fn unexpected(&mut self, expected: impl Into<String>) {
        let expected = expected.into();
        let found_kind = self.kind();
        let span = self
            .tokens
            .get(self.cursor)
            .map(|token| token.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
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
        self.record_error(error);
    }
    pub(super) fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }
    pub(super) fn expect(&mut self, kind: TokenKind) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.bump())
        } else {
            self.unexpected(kind.name());
            Err(())
        }
    }
    pub(super) fn ident_expected(&mut self, expected: &'static str) -> PResult<Ident> {
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
    pub(super) fn ident(&mut self) -> PResult<Ident> {
        self.ident_expected("identifier")
    }

    /// Parse a comma-separated list without consuming its closing delimiter.
    ///
    /// Keeping delimiter handling here gives item, type, expression, and
    /// statement grammars the same trailing-comma behavior while each module
    /// still owns the syntax of one element.
    pub(super) fn comma_separated<T>(
        &mut self,
        end: TokenKind,
        mut element: impl FnMut(&mut Self) -> PResult<T>,
    ) -> PResult<Vec<T>> {
        let mut values = Vec::new();
        if !self.at(end) {
            loop {
                values.push(element(self)?);
                if !self.eat(TokenKind::Comma) || self.at(end) {
                    break;
                }
            }
        }
        Ok(values)
    }

    pub(super) fn skip_brace_group(&mut self) -> u32 {
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

    pub(super) fn recover_item(&mut self) -> Span {
        let start = self
            .tokens
            .get(self.cursor)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::point_in_file(self.file_id, self.end_offset()));
        if self.recover_reserved_let_keywords() || self.recover_missing_item_name() {
            return start;
        }
        // Track brace depth across the skipped tokens so synchronization only
        // happens at top level: an item prefix inside the failed item's own
        // braces (a struct's later methods, say) is part of the failed item,
        // and re-parsing it as a fresh top-level item emits phantom errors at
        // valid lines (RUE-726). A stray close brace clamps to zero so text
        // after an over-closed item still synchronizes.
        // Recovery advances the cursor in place and never copies the skipped
        // token region into a diagnostic payload, so a long malformed region
        // costs constant auxiliary recovery storage (RUE-792).
        let mut brace_depth: usize = 0;
        if !self.at(TokenKind::Eof) {
            debug_assert_eq!(
                recovery::item_recovery_action(
                    recovery::ItemRecoveryPosition::Initial,
                    &self.kind(),
                    brace_depth,
                ),
                recovery::ItemRecoveryAction::Consume
            );
            match self.kind() {
                TokenKind::LBrace => brace_depth += 1,
                TokenKind::RBrace => {}
                _ => {}
            }
            self.bump();
        }
        while !self.at(TokenKind::Eof)
            && recovery::item_recovery_action(
                recovery::ItemRecoveryPosition::AfterProgress,
                &self.kind(),
                brace_depth,
            ) == recovery::ItemRecoveryAction::Consume
        {
            match self.kind() {
                TokenKind::LBrace => brace_depth += 1,
                TokenKind::RBrace => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
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
                // The grammar diagnostic that triggered recovery already
                // points at the first offending reserved keyword. Do not
                // retain a second, narrower diagnostic for that same token;
                // recovery diagnostics are for later malformed item prefixes
                // encountered while scanning the failed body.
                if let Some(expected) = expected
                    && token.span != initial_span
                {
                    self.record_error(CompileError::new(
                        ErrorKind::UnexpectedToken {
                            expected: expected.into(),
                            found: token.kind.name().to_owned().into(),
                        },
                        token.span,
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
                self.record_error(CompileError::new(
                    ErrorKind::UnexpectedToken {
                        expected: "identifier".into(),
                        found: "'fn'".into(),
                    },
                    token.span,
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
                    self.record_error(CompileError::new(
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
#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;

    fn parses(source: &str) -> bool {
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        Parser::new(tokens, interner).parse().is_ok()
    }

    #[test]
    fn shared_delimiter_and_identifier_helpers_accept_lists_and_blocks() {
        assert!(parses(
            "fn f(a: i32, b: i32,) -> i32 { let xs = [a, b,]; xs[0] }"
        ));
    }

    #[test]
    fn shared_recovery_rejects_unbalanced_delimiters() {
        assert!(!parses("fn f(a: i32 { a }"));
    }
}
