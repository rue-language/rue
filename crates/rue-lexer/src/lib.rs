#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    // Literals
    Integer(i64),

    // Keywords
    Fn,
    Let,
    If,
    Else,
    While,

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

    // Delimiters
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    Semicolon,
    Comma,

    // Special
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

pub struct Lexer<'a> {
    input: &'a str,
    position: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, position: 0 }
    }

    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();

        while !self.is_at_end() {
            self.skip_whitespace();
            if !self.is_at_end() {
                tokens.push(self.next_token());
            }
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span {
                start: self.position,
                end: self.position,
            },
        });

        tokens
    }

    fn next_token(&mut self) -> Token {
        let start = self.position;

        match self.current_char() {
            '+' => self.make_token(TokenKind::Plus, start),
            '-' => self.make_token(TokenKind::Minus, start),
            '*' => self.make_token(TokenKind::Star, start),
            '/' => self.make_token(TokenKind::Slash, start),
            '%' => self.make_token(TokenKind::Percent, start),
            '(' => self.make_token(TokenKind::LeftParen, start),
            ')' => self.make_token(TokenKind::RightParen, start),
            '{' => self.make_token(TokenKind::LeftBrace, start),
            '}' => self.make_token(TokenKind::RightBrace, start),
            ';' => self.make_token(TokenKind::Semicolon, start),
            ',' => self.make_token(TokenKind::Comma, start),
            '=' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Token {
                        kind: TokenKind::Equal,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                } else {
                    Token {
                        kind: TokenKind::Assign,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                }
            }
            '<' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Token {
                        kind: TokenKind::LessEqual,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                } else {
                    Token {
                        kind: TokenKind::Less,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                }
            }
            '>' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Token {
                        kind: TokenKind::GreaterEqual,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                } else {
                    Token {
                        kind: TokenKind::Greater,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                }
            }
            '!' => {
                self.advance();
                if self.current_char() == '=' {
                    self.advance();
                    Token {
                        kind: TokenKind::NotEqual,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    }
                } else {
                    panic!("Unexpected character '!' at position {start}");
                }
            }
            '0'..='9' => self.lex_number(start),
            'a'..='z' | 'A'..='Z' | '_' => self.lex_ident_or_keyword(start),
            c => panic!("Unexpected character '{c}' at position {start}"),
        }
    }

    fn lex_number(&mut self, start: usize) -> Token {
        while self.current_char().is_ascii_digit() {
            self.advance();
        }

        let text = &self.input[start..self.position];
        let value = text.parse::<i64>().expect("Invalid number");

        Token {
            kind: TokenKind::Integer(value),
            span: Span {
                start,
                end: self.position,
            },
        }
    }

    fn lex_ident_or_keyword(&mut self, start: usize) -> Token {
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
            _ => TokenKind::Ident(text.to_string()),
        };

        Token {
            kind,
            span: Span {
                start,
                end: self.position,
            },
        }
    }

    fn make_token(&mut self, kind: TokenKind, start: usize) -> Token {
        self.advance();
        Token {
            kind,
            span: Span {
                start,
                end: self.position,
            },
        }
    }

    fn skip_whitespace(&mut self) {
        while self.current_char().is_whitespace() {
            self.advance();
        }
    }

    fn current_char(&self) -> char {
        self.input.chars().nth(self.position).unwrap_or('\0')
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
        let tokens = lexer.tokenize();

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
        let tokens = lexer.tokenize();

        assert_eq!(tokens[0].kind, TokenKind::Fn);
        assert_eq!(tokens[1].kind, TokenKind::Ident("factorial".to_string()));
        assert_eq!(tokens[2].kind, TokenKind::LeftParen);
    }

    #[test]
    fn test_while_keyword() {
        let mut lexer = Lexer::new("while");
        let tokens = lexer.tokenize();

        assert_eq!(tokens[0].kind, TokenKind::While);
        assert_eq!(tokens[1].kind, TokenKind::Eof);
    }
}
