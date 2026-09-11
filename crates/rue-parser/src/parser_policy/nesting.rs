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
    // `else if` chains nest one `if` per link, so each link counts toward the
    // level that holds the chain. A chain ends at the `}` that closes its last
    // branch when no further `else` follows, and the level's ordinary
    // separators (`;`, `,`, ...) never see that end: a statement-position
    // `if ... else ...` is complete on its own (5.3:6). Without discharging
    // a completed chain, every sequential `if ... else ...` statement in one
    // block would leave a permanent unit behind, and the 257th such statement
    // was rejected as over-deep (RUE-1107). Sequential chains do not nest, so
    // a completed chain keeps only the deepest one seen since the last
    // separator, which stays an upper bound on the AST depth that follows.
    let mut chain = vec![0usize];
    let mut deepest_chain = vec![0usize];
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

    for (index, token) in tokens.iter().enumerate() {
        let kind = &token.kind;
        // A block closed on the previous token; if this token does not extend
        // an `else` chain, the chain that ended there is complete.
        if matches!(prev, Some(TokenKind::RBrace)) && !matches!(kind, TokenKind::Else) {
            let level = levels.len() - 1;
            let completed = chain[level];
            if completed > 0 {
                let kept = deepest_chain[level].max(completed);
                total_ops -= completed + deepest_chain[level];
                total_ops += kept;
                deepest_chain[level] = kept;
                chain[level] = 0;
            }
        }
        match kind {
            TokenKind::LParen | TokenKind::LBrace => {
                levels.push(0);
                chain.push(0);
                deepest_chain.push(0);
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
                chain.push(0);
                deepest_chain.push(0);
                reject_if_too_deep!(token.span);
            }
            TokenKind::RParen | TokenKind::RBrace | TokenKind::RBracket => {
                if levels.len() > 1 {
                    total_ops -= levels.pop().unwrap();
                    total_ops -= chain.pop().unwrap();
                    total_ops -= deepest_chain.pop().unwrap();
                }
            }
            TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::FatArrow
            | TokenKind::Eq
            | TokenKind::Colon
            | TokenKind::Arrow => {
                let level = levels.len() - 1;
                total_ops -= levels[level] + chain[level] + deepest_chain[level];
                levels[level] = 0;
                chain[level] = 0;
                deepest_chain[level] = 0;
            }
            // Only an `else if` link nests: a plain `else { ... }` is the
            // second child of the one `if` node and its block is counted on
            // its own `{`.
            TokenKind::Else => {
                if tokens
                    .get(index + 1)
                    .is_some_and(|next| next.kind == TokenKind::If)
                {
                    *chain.last_mut().unwrap() += 1;
                    total_ops += 1;
                    reject_if_too_deep!(token.span);
                }
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
    fn sequential_if_else_statements_do_not_accumulate_depth() {
        // `if c { } else { }` repeated: each statement is complete at its
        // closing brace with no separator, and none of them nests in the
        // next (RUE-1107).
        let file = FileId::new(4);
        let statement = [
            TokenKind::If,
            TokenKind::True,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::Else,
            TokenKind::LBrace,
            TokenKind::RBrace,
        ];
        let input = tokens(
            (0..MAX_NESTING_DEPTH + 20).flat_map(|_| statement.clone()),
            file,
        );
        assert!(check_nesting_depth(&input).is_none());
    }

    #[test]
    fn sequential_else_if_chains_keep_only_the_deepest() {
        // Two `else if` links per statement, repeated well past the limit:
        // a completed chain contributes its own depth once, never per
        // statement.
        let file = FileId::new(5);
        let statement = [
            TokenKind::If,
            TokenKind::True,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::Else,
            TokenKind::If,
            TokenKind::True,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::Else,
            TokenKind::If,
            TokenKind::True,
            TokenKind::LBrace,
            TokenKind::RBrace,
        ];
        let input = tokens(
            (0..MAX_NESTING_DEPTH + 20).flat_map(|_| statement.clone()),
            file,
        );
        assert!(check_nesting_depth(&input).is_none());
    }

    #[test]
    fn one_else_if_chain_still_counts_its_links() {
        // A single chain of `else if` links past the limit nests one `if`
        // per link and is still rejected.
        let file = FileId::new(6);
        let mut kinds = vec![
            TokenKind::If,
            TokenKind::True,
            TokenKind::LBrace,
            TokenKind::RBrace,
        ];
        for _ in 0..MAX_NESTING_DEPTH + 2 {
            kinds.extend([
                TokenKind::Else,
                TokenKind::If,
                TokenKind::True,
                TokenKind::LBrace,
                TokenKind::RBrace,
            ]);
        }
        let input = tokens(kinds, file);
        assert!(check_nesting_depth(&input).is_some());
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
