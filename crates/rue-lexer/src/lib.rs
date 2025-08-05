#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Lexical error at position {position}: {message}")]
pub struct LexError {
    pub message: String,
    pub position: usize,
}

pub type LexResult<T> = Result<T, LexError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Literals
    Integer(i64),
    Unit,

    // Keywords
    Fn,
    Let,
    If,
    Else,
    While,
    Return,
    I32,
    I64,
    Bool,
    True,
    False,
    Struct,

    // Identifiers
    Ident(String),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    Arrow,
    Colon,

    // Delimiters
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Semicolon,
    Comma,
    Dot,

    // Special
    Comment(String),
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    /// Create a dummy span for testing
    pub fn dummy() -> Self {
        Span { start: 0, end: 0 }
    }
}

/// Formats an error message with source context
pub fn format_error_with_context(
    source: &str,
    span: Span,
    message: &str,
    error_type: &str,
) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut line_start = 0;
    let mut line_num = 0;
    let mut col_num = 0;

    // Find line and column for the error position
    for (idx, line) in lines.iter().enumerate() {
        let line_end = line_start + line.len();
        if span.start >= line_start && span.start <= line_end {
            line_num = idx + 1;
            col_num = span.start - line_start + 1;
            break;
        }
        line_start = line_end + 1; // +1 for newline
    }

    // Build the error message
    let mut result = format!("{error_type}: {message}\n");

    // Add location info
    result.push_str(&format!(" --> {}:{}:{}\n", "input", line_num, col_num));

    // Add source context
    if line_num > 0 && line_num <= lines.len() {
        let line_idx = line_num - 1;
        let line = lines[line_idx];

        // Line number padding
        let line_num_str = line_num.to_string();
        let padding = " ".repeat(line_num_str.len());

        result.push_str(&format!("  {padding} |\n"));
        result.push_str(&format!("{line_num_str} | {line}\n"));

        // Add underline
        let underline_start = col_num - 1;
        let underline_len = (span.end - span.start).max(1);

        result.push_str(&format!(
            "  {} |{}{}",
            padding,
            " ".repeat(underline_start),
            "^".repeat(underline_len)
        ));

        // Add label on same line as carets
        if !message.is_empty() {
            result.push_str(&format!(" {message}"));
        }
    }

    result
}

pub struct Lexer<'a> {
    input: &'a str,
    position: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, position: 0 }
    }

    pub fn tokenize(&mut self) -> LexResult<Vec<Token>> {
        let mut tokens = Vec::new();

        while !self.is_at_end() {
            // Handle whitespace and collect any comment tokens
            let comment_tokens = self.skip_whitespace_and_collect_comments()?;
            tokens.extend(comment_tokens);

            if !self.is_at_end() {
                tokens.push(self.next_token()?);
            }
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span {
                start: self.position,
                end: self.position,
            },
        });

        Ok(tokens)
    }

    fn next_token(&mut self) -> LexResult<Token> {
        let start = self.position;

        match self.current_char() {
            '+' => self.make_token(TokenKind::Plus, start),
            '-' => {
                self.advance();
                if self.current_char() == '>' {
                    self.advance();
                    Ok(Token {
                        kind: TokenKind::Arrow,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                } else if self.current_char().is_ascii_digit() {
                    // Handle negative number literal
                    self.lex_number(start)
                } else {
                    Ok(Token {
                        kind: TokenKind::Minus,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                }
            }
            '*' => self.make_token(TokenKind::Star, start),
            '/' => {
                // Check if it's a comment start, but since comments are handled
                // in skip_whitespace, if we get here it's a division operator
                self.make_token(TokenKind::Slash, start)
            }
            '%' => self.make_token(TokenKind::Percent, start),
            ':' => self.make_token(TokenKind::Colon, start),
            '(' => self.make_token(TokenKind::LeftParen, start),
            ')' => self.make_token(TokenKind::RightParen, start),
            '{' => self.make_token(TokenKind::LeftBrace, start),
            '}' => self.make_token(TokenKind::RightBrace, start),
            '[' => self.make_token(TokenKind::LeftBracket, start),
            ']' => self.make_token(TokenKind::RightBracket, start),
            ';' => self.make_token(TokenKind::Semicolon, start),
            ',' => self.make_token(TokenKind::Comma, start),
            '.' => self.make_token(TokenKind::Dot, start),
            '=' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Ok(Token {
                        kind: TokenKind::Equal,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                } else {
                    Ok(Token {
                        kind: TokenKind::Assign,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                }
            }
            '<' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Ok(Token {
                        kind: TokenKind::LessEqual,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                } else {
                    Ok(Token {
                        kind: TokenKind::Less,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                }
            }
            '>' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Ok(Token {
                        kind: TokenKind::GreaterEqual,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                } else {
                    Ok(Token {
                        kind: TokenKind::Greater,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                }
            }
            '!' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Ok(Token {
                        kind: TokenKind::NotEqual,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    })
                } else {
                    Err(LexError {
                        message: "Unexpected character '!'. Did you mean '!='?".to_string(),
                        position: start,
                    })
                }
            }
            '0'..='9' => self.lex_number(start),
            'a'..='z' | 'A'..='Z' | '_' => self.lex_ident_or_keyword(start),
            c => Err(LexError {
                message: format!("Unexpected character '{c}'"),
                position: start,
            }),
        }
    }

    fn lex_number(&mut self, start: usize) -> LexResult<Token> {
        while self.current_char().is_ascii_digit() {
            self.advance();
        }

        let text = &self.input[start..self.position];
        let value = text.parse::<i64>().map_err(|_| LexError {
            message: format!("Invalid number: '{text}'"),
            position: start,
        })?;

        Ok(Token {
            kind: TokenKind::Integer(value),
            span: Span {
                start,
                end: self.position,
            },
        })
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> LexResult<Token> {
        while self.current_char().is_alphanumeric() || self.current_char() == '_' {
            self.advance();
        }

        let text = &self.input[start..self.position];
        let kind = match text {
            "fn" => TokenKind::Fn,
            "let" => TokenKind::Let,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "return" => TokenKind::Return,
            "i32" => TokenKind::I32,
            "i64" => TokenKind::I64,
            "bool" => TokenKind::Bool,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "struct" => TokenKind::Struct,
            _ => TokenKind::Ident(text.to_string()),
        };

        Ok(Token {
            kind,
            span: Span {
                start,
                end: self.position,
            },
        })
    }

    fn make_token(&mut self, kind: TokenKind, start: usize) -> LexResult<Token> {
        self.advance();
        Ok(Token {
            kind,
            span: Span {
                start,
                end: self.position,
            },
        })
    }

    fn skip_whitespace_and_collect_comments(&mut self) -> LexResult<Vec<Token>> {
        let mut comment_tokens = Vec::new();

        loop {
            if self.current_char().is_whitespace() {
                self.advance();
            } else if self.current_char() == '/' && self.peek_char() == '/' {
                let token = self.lex_single_line_comment()?;
                comment_tokens.push(token);
            } else if self.current_char() == '/' && self.peek_char() == '*' {
                let token = self.lex_multi_line_comment()?;
                comment_tokens.push(token);
            } else {
                break;
            }
        }
        Ok(comment_tokens)
    }

    fn lex_single_line_comment(&mut self) -> LexResult<Token> {
        let start = self.position;
        self.advance(); // Skip first '/'
        self.advance(); // Skip second '/'

        let content_start = self.position;
        while !self.is_at_end() && self.current_char() != '\n' {
            self.advance();
        }

        let comment_content = self.input[content_start..self.position].to_string();

        Ok(Token {
            kind: TokenKind::Comment(comment_content),
            span: Span {
                start,
                end: self.position,
            },
        })
    }

    fn lex_multi_line_comment(&mut self) -> LexResult<Token> {
        let start = self.position;
        self.advance(); // Skip '/'
        self.advance(); // Skip '*'

        let content_start = self.position;
        let mut depth = 1;

        while !self.is_at_end() && depth > 0 {
            if self.current_char() == '/' && self.peek_char() == '*' {
                depth += 1;
                self.advance();
                self.advance();
            } else if self.current_char() == '*' && self.peek_char() == '/' {
                depth -= 1;
                self.advance();
                self.advance();
            } else {
                self.advance();
            }
        }

        if depth > 0 {
            return Err(LexError {
                message: "Unterminated multi-line comment".to_string(),
                position: start,
            });
        }

        // Extract comment content, excluding the outer /* */
        let content_end = self.position - 2; // before the closing */
        let comment_content = self.input[content_start..content_end].to_string();

        Ok(Token {
            kind: TokenKind::Comment(comment_content),
            span: Span {
                start,
                end: self.position,
            },
        })
    }

    fn peek_char(&self) -> char {
        let bytes = self.input.as_bytes();
        if self.position >= bytes.len() {
            return '\0';
        }

        // Get the current character to find its byte length
        let current_char = self.current_char();
        let next_pos = self.position + current_char.len_utf8();

        if next_pos >= bytes.len() {
            return '\0';
        }

        // Safe because we know this is a valid UTF-8 boundary
        self.input[next_pos..].chars().next().unwrap_or('\0')
    }

    fn current_char(&self) -> char {
        if self.position >= self.input.len() {
            return '\0';
        }
        // Safe because position is always at a valid UTF-8 boundary
        self.input[self.position..].chars().next().unwrap_or('\0')
    }

    fn advance(&mut self) {
        if !self.is_at_end() {
            self.position += self.current_char().len_utf8();
        }
    }

    fn is_at_end(&self) -> bool {
        self.position >= self.input.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tokens() {
        let mut lexer = Lexer::new("+ - * / %");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Plus);
        assert_eq!(tokens[1].kind, TokenKind::Minus);
        assert_eq!(tokens[2].kind, TokenKind::Star);
        assert_eq!(tokens[3].kind, TokenKind::Slash);
        assert_eq!(tokens[4].kind, TokenKind::Percent);
        assert_eq!(tokens[5].kind, TokenKind::Eof);
    }

    #[test]
    fn test_factorial() {
        let input = r#"
fn factorial(n) {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}
        "#;

        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Fn);
        assert_eq!(tokens[1].kind, TokenKind::Ident("factorial".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::LeftParen);
    }

    #[test]
    fn test_while_keyword() {
        let mut lexer = Lexer::new("while");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::While);
        assert_eq!(tokens[1].kind, TokenKind::Eof);
    }

    #[test]
    fn test_type_keywords() {
        let mut lexer = Lexer::new("i32 i64 bool true false");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::I32);
        assert_eq!(tokens[1].kind, TokenKind::I64);
        assert_eq!(tokens[2].kind, TokenKind::Bool);
        assert_eq!(tokens[3].kind, TokenKind::True);
        assert_eq!(tokens[4].kind, TokenKind::False);
        assert_eq!(tokens[5].kind, TokenKind::Eof);
    }

    #[test]
    fn test_arrow_and_colon() {
        let mut lexer = Lexer::new("-> : - >");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Arrow);
        assert_eq!(tokens[1].kind, TokenKind::Colon);
        assert_eq!(tokens[2].kind, TokenKind::Minus);
        assert_eq!(tokens[3].kind, TokenKind::Greater);
        assert_eq!(tokens[4].kind, TokenKind::Eof);
    }

    #[test]
    fn test_typed_function() {
        let mut lexer = Lexer::new("fn add(a: i32, b: i32) -> i32");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Fn);
        assert_eq!(tokens[1].kind, TokenKind::Ident("add".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::LeftParen);
        assert_eq!(tokens[3].kind, TokenKind::Ident("a".to_string()));
        assert_eq!(tokens[4].kind, TokenKind::Colon);
        assert_eq!(tokens[5].kind, TokenKind::I32);
        assert_eq!(tokens[6].kind, TokenKind::Comma);
        assert_eq!(tokens[7].kind, TokenKind::Ident("b".to_string()));
        assert_eq!(tokens[8].kind, TokenKind::Colon);
        assert_eq!(tokens[9].kind, TokenKind::I32);
        assert_eq!(tokens[10].kind, TokenKind::RightParen);
        assert_eq!(tokens[11].kind, TokenKind::Arrow);
        assert_eq!(tokens[12].kind, TokenKind::I32);
        assert_eq!(tokens[13].kind, TokenKind::Eof);
    }

    #[test]
    fn test_single_line_comment() {
        let mut lexer = Lexer::new("// This is a comment\n42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" This is a comment".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_single_line_comment_at_eof() {
        let mut lexer = Lexer::new("42 // Comment at EOF");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Integer(42));
        assert_eq!(
            tokens[1].kind,
            TokenKind::Comment(" Comment at EOF".to_string())
        );
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_multiple_single_line_comments() {
        let mut lexer = Lexer::new("// Comment 1\n42\n// Comment 2\n+ 3");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Comment(" Comment 1".to_string()));
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Comment(" Comment 2".to_string()));
        assert_eq!(tokens[3].kind, TokenKind::Plus);
        assert_eq!(tokens[4].kind, TokenKind::Integer(3));
        assert_eq!(tokens[5].kind, TokenKind::Eof);
    }

    #[test]
    fn test_empty_single_line_comment() {
        let mut lexer = Lexer::new("//\n42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Comment("".to_string()));
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_basic_multi_line_comment() {
        let mut lexer = Lexer::new("/* comment */ 42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Comment(" comment ".to_string()));
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_multi_line_comment_spanning_lines() {
        let mut lexer = Lexer::new("42 /* comment\nspanning\nmultiple lines */ + 3");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Integer(42));
        assert_eq!(
            tokens[1].kind,
            TokenKind::Comment(" comment\nspanning\nmultiple lines ".to_string())
        );
        assert_eq!(tokens[2].kind, TokenKind::Plus);
        assert_eq!(tokens[3].kind, TokenKind::Integer(3));
        assert_eq!(tokens[4].kind, TokenKind::Eof);
    }

    #[test]
    fn test_empty_multi_line_comment() {
        let mut lexer = Lexer::new("/**/ 42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Comment("".to_string()));
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_nested_multi_line_comments() {
        let mut lexer = Lexer::new("/* outer /* inner */ still comment */ 42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" outer /* inner */ still comment ".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_deeply_nested_comments() {
        let mut lexer = Lexer::new("/* a /* b /* c */ b */ a */ 42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" a /* b /* c */ b */ a ".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_unterminated_comment() {
        let mut lexer = Lexer::new("/* unterminated");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unterminated multi-line comment"));
    }

    #[test]
    fn test_unterminated_nested_comment() {
        let mut lexer = Lexer::new("/* outer /* inner */");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unterminated multi-line comment"));
    }

    #[test]
    fn test_mixed_comments() {
        let mut lexer = Lexer::new("// single\n/* multi */ 42 // end");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Comment(" single".to_string()));
        assert_eq!(tokens[1].kind, TokenKind::Comment(" multi ".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::Integer(42));
        assert_eq!(tokens[3].kind, TokenKind::Comment(" end".to_string()));
        assert_eq!(tokens[4].kind, TokenKind::Eof);
    }

    #[test]
    fn test_comments_between_tokens() {
        let mut lexer = Lexer::new("42 /* comment */ + /* another */ 3");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Integer(42));
        assert_eq!(tokens[1].kind, TokenKind::Comment(" comment ".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::Plus);
        assert_eq!(tokens[3].kind, TokenKind::Comment(" another ".to_string()));
        assert_eq!(tokens[4].kind, TokenKind::Integer(3));
        assert_eq!(tokens[5].kind, TokenKind::Eof);
    }

    #[test]
    fn test_single_line_in_multi_line() {
        let mut lexer = Lexer::new("/* // this is still a comment */ 42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" // this is still a comment ".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_multi_line_markers_in_single_line() {
        let mut lexer = Lexer::new("// /* */ still a comment\n42");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" /* */ still a comment".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_division_not_comment() {
        let mut lexer = Lexer::new("42 / 6");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Integer(42));
        assert_eq!(tokens[1].kind, TokenKind::Slash);
        assert_eq!(tokens[2].kind, TokenKind::Integer(6));
        assert_eq!(tokens[3].kind, TokenKind::Eof);
    }

    #[test]
    fn test_negative_number_literal() {
        let mut lexer = Lexer::new("-5");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Integer(-5));
        assert_eq!(tokens[1].kind, TokenKind::Eof);
    }

    #[test]
    fn test_negative_number_in_expression() {
        let mut lexer = Lexer::new("let x = -42;");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Let);
        assert_eq!(tokens[1].kind, TokenKind::Ident("x".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::Assign);
        assert_eq!(tokens[3].kind, TokenKind::Integer(-42));
        assert_eq!(tokens[4].kind, TokenKind::Semicolon);
        assert_eq!(tokens[5].kind, TokenKind::Eof);
    }

    #[test]
    fn test_subtraction_vs_negative() {
        let mut lexer = Lexer::new("5 - 3");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Integer(5));
        assert_eq!(tokens[1].kind, TokenKind::Minus);
        assert_eq!(tokens[2].kind, TokenKind::Integer(3));
        assert_eq!(tokens[3].kind, TokenKind::Eof);
    }

    #[test]
    fn test_negative_with_spaces() {
        let mut lexer = Lexer::new("- 5");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Minus);
        assert_eq!(tokens[1].kind, TokenKind::Integer(5));
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_unit_literal_tokenization() {
        let mut lexer = Lexer::new("()");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::LeftParen);
        assert_eq!(tokens[1].kind, TokenKind::RightParen);
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_unit_literal_with_spaces() {
        let mut lexer = Lexer::new("( )");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::LeftParen);
        assert_eq!(tokens[1].kind, TokenKind::RightParen);
        assert_eq!(tokens[2].kind, TokenKind::Eof);
    }

    #[test]
    fn test_unit_literal_in_expression() {
        let mut lexer = Lexer::new("let x = ();");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Let);
        assert_eq!(tokens[1].kind, TokenKind::Ident("x".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::Assign);
        assert_eq!(tokens[3].kind, TokenKind::LeftParen);
        assert_eq!(tokens[4].kind, TokenKind::RightParen);
        assert_eq!(tokens[5].kind, TokenKind::Semicolon);
        assert_eq!(tokens[6].kind, TokenKind::Eof);
    }

    #[test]
    fn test_unexpected_character() {
        let mut lexer = Lexer::new("let x = @");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unexpected character '@'"));
        assert_eq!(err.position, 8);
    }

    #[test]
    fn test_unexpected_bang() {
        let mut lexer = Lexer::new("let x = !y");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Unexpected character '!'"));
        assert!(err.message.contains("Did you mean '!='?"));
    }

    #[test]
    fn test_i64_max_literal() {
        let mut lexer = Lexer::new("9223372036854775807");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer(9223372036854775807));
        assert_eq!(tokens[1].kind, TokenKind::Eof);
    }

    #[test]
    fn test_i64_min_literal() {
        // i64::MIN is -9223372036854775808
        let mut lexer = Lexer::new("-9223372036854775808");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer(-9223372036854775808));
        assert_eq!(tokens[1].kind, TokenKind::Eof);
    }

    #[test]
    fn test_literal_overflow_positive() {
        // Number larger than i64::MAX
        let mut lexer = Lexer::new("9223372036854775808");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Invalid number"));
    }

    #[test]
    fn test_literal_overflow_negative() {
        // Number smaller than i64::MIN
        let mut lexer = Lexer::new("-9223372036854775809");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Invalid number"));
    }

    #[test]
    fn test_literal_overflow_very_large() {
        // Very large number
        let mut lexer = Lexer::new("99999999999999999999999999");
        let result = lexer.tokenize();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("Invalid number"));
    }

    #[test]
    fn test_literal_edge_cases() {
        // Test various edge cases
        let mut lexer = Lexer::new("0");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer(0));

        let mut lexer = Lexer::new("-0");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer(0));

        // i32::MAX
        let mut lexer = Lexer::new("2147483647");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer(2147483647));

        // i32::MIN
        let mut lexer = Lexer::new("-2147483648");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Integer(-2147483648));
    }

    #[test]
    fn test_unicode_in_single_line_comments() {
        // Test Japanese characters
        let mut lexer = Lexer::new("// こんにちは世界\n42;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4); // comment, 42, ;, EOF
        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" こんにちは世界".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Semicolon);
        assert_eq!(tokens[3].kind, TokenKind::Eof);

        // Test Chinese characters
        let mut lexer = Lexer::new("// 你好世界\n100;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[0].kind, TokenKind::Comment(" 你好世界".to_string()));
        assert_eq!(tokens[1].kind, TokenKind::Integer(100));

        // Test Arabic characters
        let mut lexer = Lexer::new("// مرحبا بالعالم\n200;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4);
        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" مرحبا بالعالم".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(200));

        // Test emojis
        let mut lexer = Lexer::new("// 🚀🎉🎊🎈🎁🎂\n300;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4);
        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" 🚀🎉🎊🎈🎁🎂".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(300));

        // Test box drawing characters
        let mut lexer = Lexer::new("// ┌─────────┐\n400;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4);
        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" ┌─────────┐".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(400));

        // Test accented characters
        let mut lexer = Lexer::new("// café, naïve, résumé\n500;");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4);
        assert_eq!(
            tokens[0].kind,
            TokenKind::Comment(" café, naïve, résumé".to_string())
        );
        assert_eq!(tokens[1].kind, TokenKind::Integer(500));
    }

    #[test]
    fn test_unicode_in_multi_line_comments() {
        // Test various unicode in multi-line comments
        let input = r#"/* 
        日本語のコメント
        中文注释
        تعليق عربي
        🎯🎪🎨
        */ 
        42;"#;

        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4); // comment, 42, ;, EOF
        assert!(matches!(tokens[0].kind, TokenKind::Comment(_)));
        assert_eq!(tokens[1].kind, TokenKind::Integer(42));
        assert_eq!(tokens[2].kind, TokenKind::Semicolon);
        assert_eq!(tokens[3].kind, TokenKind::Eof);

        // Test nested comments with unicode
        let input2 = r#"/* outer /* inner with unicode: 你好 */ back to outer */ 100;"#;
        let mut lexer = Lexer::new(input2);
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4);
        assert!(matches!(tokens[0].kind, TokenKind::Comment(_)));
        assert_eq!(tokens[1].kind, TokenKind::Integer(100));
    }

    #[test]
    fn test_new_aggregate_tokens() {
        let mut lexer = Lexer::new("struct Point { x: [i32; 5], } arr[0].field");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens[0].kind, TokenKind::Struct);
        assert_eq!(tokens[1].kind, TokenKind::Ident("Point".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::LeftBrace);
        assert_eq!(tokens[3].kind, TokenKind::Ident("x".to_string()));
        assert_eq!(tokens[4].kind, TokenKind::Colon);
        assert_eq!(tokens[5].kind, TokenKind::LeftBracket);
        assert_eq!(tokens[6].kind, TokenKind::I32);
        assert_eq!(tokens[7].kind, TokenKind::Semicolon);
        assert_eq!(tokens[8].kind, TokenKind::Integer(5));
        assert_eq!(tokens[9].kind, TokenKind::RightBracket);
        assert_eq!(tokens[10].kind, TokenKind::Comma);
        assert_eq!(tokens[11].kind, TokenKind::RightBrace);
        assert_eq!(tokens[12].kind, TokenKind::Ident("arr".to_string()));
        assert_eq!(tokens[13].kind, TokenKind::LeftBracket);
        assert_eq!(tokens[14].kind, TokenKind::Integer(0));
        assert_eq!(tokens[15].kind, TokenKind::RightBracket);
        assert_eq!(tokens[16].kind, TokenKind::Dot);
        assert_eq!(tokens[17].kind, TokenKind::Ident("field".to_string()));
        assert_eq!(tokens[18].kind, TokenKind::Eof);
    }

    #[test]
    fn test_unicode_mixed_with_code() {
        // Test unicode in comments mixed with regular code
        let input = r#"
        let x = 10; // 设置x的值
        let y = 20; /* مجموع */
        let z = x + y; // 計算する
        z; // النتيجة
        "#;

        let mut lexer = Lexer::new(input);
        let tokens = lexer.tokenize().unwrap();

        // Verify we get the expected tokens (ignoring comments)
        let non_comment_non_eof_tokens: Vec<_> = tokens
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Eof | TokenKind::Comment(_)))
            .collect();

        assert_eq!(non_comment_non_eof_tokens.len(), 19); // let x = 10 ; let y = 20 ; let z = x + y ; z ;

        // Check a few key tokens
        assert_eq!(non_comment_non_eof_tokens[0].kind, TokenKind::Let);
        assert_eq!(
            non_comment_non_eof_tokens[1].kind,
            TokenKind::Ident("x".to_string())
        );
        assert_eq!(non_comment_non_eof_tokens[3].kind, TokenKind::Integer(10));
    }
}
