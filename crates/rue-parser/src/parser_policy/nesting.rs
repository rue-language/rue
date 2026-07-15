//! Conservative syntax-tree depth guard over Rue tokens and source spans.
//!
//! The guard runs before AST construction because parsing, lowering, and AST
//! destruction all recurse through syntax shape. Its tracked value is an upper
//! bound: open delimiters and expression/type wrappers increase depth, while
//! separators reset completed subexpressions. Over-counting is safe; under-
//! counting is not. Array opens are distinguished from postfix indexes so a
//! nested array type consumes one unit of the documented depth allowance per
//! level. Every parser implementation must apply this policy before building an
//! AST and preserve the triggering token's complete Rue span.

use rue_error::{CompileError, ErrorKind, MAX_NESTING_DEPTH};
use rue_lexer::{Token, TokenKind};

/// Reject input whose token shape can produce an AST deeper than the compiler's
/// recursive parser, lowering, and drop paths safely support.
pub(crate) fn check_nesting_depth(tokens: &[Token]) -> Option<CompileError> {
    let mut levels = vec![0usize];
    let mut total_ops = 0usize;
    let mut prev: Option<&TokenKind> = None;

    macro_rules! reject_if_too_deep {
        ($span:expr) => {
            if (levels.len() - 1) + total_ops > MAX_NESTING_DEPTH {
                return Some(CompileError::new(
                    ErrorKind::NestingLimitExceeded {
                        limit: MAX_NESTING_DEPTH,
                    },
                    $span,
                ));
            }
        };
    }

    for token in tokens {
        let kind = &token.kind;
        match kind {
            TokenKind::LParen | TokenKind::LBrace => {
                levels.push(0);
                reject_if_too_deep!(token.span);
            }
            TokenKind::LBracket => {
                let is_postfix_index = matches!(
                    prev,
                    Some(
                        TokenKind::Ident(_)
                            | TokenKind::Int(_)
                            | TokenKind::String(_)
                            | TokenKind::True
                            | TokenKind::False
                            | TokenKind::SelfValue
                            | TokenKind::RParen
                            | TokenKind::RBracket
                            | TokenKind::RBrace
                    )
                );
                if is_postfix_index {
                    *levels.last_mut().unwrap() += 1;
                    total_ops += 1;
                }
                levels.push(0);
                reject_if_too_deep!(token.span);
            }
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                if levels.len() > 1 {
                    total_ops -= levels.pop().unwrap();
                }
            }
            TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::FatArrow
            | TokenKind::Eq
            | TokenKind::Colon
            | TokenKind::Arrow => {
                total_ops -= *levels.last().unwrap();
                *levels.last_mut().unwrap() = 0;
            }
            TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Lt
            | TokenKind::Gt
            | TokenKind::LtEq
            | TokenKind::GtEq
            | TokenKind::AmpAmp
            | TokenKind::PipePipe
            | TokenKind::Amp
            | TokenKind::Pipe
            | TokenKind::Caret
            | TokenKind::Tilde
            | TokenKind::LtLt
            | TokenKind::GtGt
            | TokenKind::Bang
            | TokenKind::Dot
            | TokenKind::Question
            | TokenKind::Else
            | TokenKind::Ptr => {
                *levels.last_mut().unwrap() += 1;
                total_ops += 1;
                reject_if_too_deep!(token.span);
            }
            _ => {}
        }
        prev = Some(kind);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_span::{FileId, Span};

    fn tokens(kinds: impl IntoIterator<Item = TokenKind>, file: FileId) -> Vec<Token> {
        kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| Token {
                kind,
                span: Span::with_file(file, index as u32, index as u32 + 1),
            })
            .collect()
    }

    #[test]
    fn shallow_valid_and_malformed_streams_are_accepted() {
        let file = FileId::new(3);
        assert!(
            check_nesting_depth(&tokens(
                [TokenKind::LParen, TokenKind::Int(1), TokenKind::RParen],
                file
            ))
            .is_none()
        );
        assert!(
            check_nesting_depth(&tokens(
                [TokenKind::RBrace, TokenKind::LParen, TokenKind::Semi],
                file
            ))
            .is_none()
        );
    }

    #[test]
    fn excessive_depth_reports_trigger_span_and_file() {
        let file = FileId::new(9);
        let input = tokens(
            std::iter::repeat_n(TokenKind::LParen, MAX_NESTING_DEPTH + 2),
            file,
        );
        let error = check_nesting_depth(&input).unwrap();
        assert_eq!(error.span(), Some(input[MAX_NESTING_DEPTH].span));
        assert_eq!(error.span().unwrap().file_id, file);
    }

    #[test]
    fn separators_reset_operator_depth() {
        let file = FileId::new(1);
        let input = tokens(
            (0..MAX_NESTING_DEPTH + 20).flat_map(|_| [TokenKind::Bang, TokenKind::Semi]),
            file,
        );
        assert!(check_nesting_depth(&input).is_none());
    }
}
