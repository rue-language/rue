//! Turns parser events back into a lossless green tree.

use crate::green::TokenCache;
use crate::input::{Input, lexer_error};
use crate::parser::Event;
use crate::{GreenElement, GreenNode, SyntaxError, SyntaxKind, SyntaxKind::*, TextRange};
use std::mem;

/// A parsed window: its tree and its errors, with ranges relative to the
/// window's start.
pub(crate) struct Built {
    pub(crate) node: GreenNode,
    pub(crate) errors: Vec<SyntaxError>,
}

/// Builds the tree for one window of `input`.
///
/// `raw_start..raw_end` is the window's raw token range. The window's trivia
/// is attached as follows: trivia before a node's first token belongs to the
/// enclosing node, trivia inside a node stays inside, and trivia left at the
/// end belongs to the root. A window that is a delimited group starts and
/// ends with its delimiters, so the attachment inside it does not depend on
/// whether it was parsed inline or separately.
pub(crate) fn build(
    input: &Input<'_>,
    raw_start: usize,
    raw_end: usize,
    events: Vec<Event>,
    deferred: Vec<Option<Built>>,
    deferred_raw_ends: &[usize],
) -> Built {
    TOKEN_CACHE.with_borrow_mut(|cache| {
        build_with_cache(
            input,
            raw_start,
            raw_end,
            events,
            deferred,
            deferred_raw_ends,
            cache,
        )
    })
}

thread_local! {
    /// Each thread's token cache, shared by every tree it builds. Green
    /// tokens are immutable, so sharing them across trees is free; it makes
    /// building a small deferred group as cheap as building it inline.
    static TOKEN_CACHE: std::cell::RefCell<TokenCache> = Default::default();
}

fn build_with_cache(
    input: &Input<'_>,
    raw_start: usize,
    raw_end: usize,
    mut events: Vec<Event>,
    mut deferred: Vec<Option<Built>>,
    deferred_raw_ends: &[usize],
    cache: &mut TokenCache,
) -> Built {
    cache.trim();
    let mut builder = Builder {
        input,
        raw_pos: raw_start,
        raw_end,
        base: input.raw_starts[raw_start],
        stack: Vec::new(),
        children: Vec::new(),
        errors: Vec::new(),
        cache,
        last_token: TextRange::default(),
    };

    let mut forward_kinds = Vec::new();
    for i in 0..events.len() {
        match mem::replace(&mut events[i], Event::Finish) {
            Event::Start {
                kind: TOMBSTONE,
                forward_parent: None,
            } => {
                // An abandoned marker: it has no Finish. Restore the slot so a
                // later forward-parent walk never lands on a fake Finish.
                events[i] = Event::Start {
                    kind: TOMBSTONE,
                    forward_parent: None,
                };
            }
            Event::Start {
                kind,
                forward_parent,
            } => {
                // Collect this node and every node that must enclose it, then
                // open them outermost first.
                forward_kinds.push(kind);
                let mut idx = i;
                let mut fp = forward_parent;
                while let Some(delta) = fp {
                    idx += delta as usize;
                    match mem::replace(
                        &mut events[idx],
                        Event::Start {
                            kind: TOMBSTONE,
                            forward_parent: None,
                        },
                    ) {
                        Event::Start {
                            kind,
                            forward_parent,
                        } => {
                            forward_kinds.push(kind);
                            fp = forward_parent;
                        }
                        _ => unreachable!("forward parents point at Start events"),
                    }
                }
                for kind in forward_kinds.drain(..).rev() {
                    if kind != TOMBSTONE {
                        builder.start_node(kind);
                    }
                }
            }
            Event::Finish => builder.finish_node(),
            Event::Token { kind, n_raw } => builder.token(kind, n_raw as usize),
            Event::FloatPiece { piece, last } => builder.float_piece(piece, last),
            Event::Error {
                message,
                on_previous_token,
            } => builder.error(message, on_previous_token),
            Event::Deferred { index } => {
                let built = deferred[index as usize]
                    .take()
                    .expect("each deferred group is spliced once");
                builder.splice(built, deferred_raw_ends[index as usize]);
            }
        }
    }

    assert!(builder.stack.is_empty(), "unbalanced node events");
    assert_eq!(
        builder.children.len(),
        1,
        "a window builds exactly one root"
    );
    let node = match builder.children.pop() {
        Some(GreenElement::Node(node)) => node,
        _ => unreachable!("the root is a node"),
    };
    Built {
        node,
        errors: builder.errors,
    }
}

/// Delimiter matching turns an unmatched closer into an `ERROR` token.
fn is_closer(kind: rue_rust_lexer::TokenKind) -> bool {
    use rue_rust_lexer::TokenKind;
    matches!(
        kind,
        TokenKind::CloseParen | TokenKind::CloseBracket | TokenKind::CloseBrace
    )
}

struct Builder<'a, 't, 'c> {
    input: &'a Input<'t>,
    raw_pos: usize,
    raw_end: usize,
    /// The absolute offset of the window's start.
    base: u32,
    /// Open nodes: their kind and where their children start in `children`.
    stack: Vec<(SyntaxKind, usize)>,
    children: Vec<GreenElement>,
    errors: Vec<SyntaxError>,
    cache: &'c mut TokenCache,
    /// The range of the last token pushed, relative to the window start.
    last_token: TextRange,
}

impl Builder<'_, '_, '_> {
    /// The current offset relative to the window start.
    fn offset(&self) -> u32 {
        self.input.raw_starts[self.raw_pos] - self.base
    }

    fn start_node(&mut self, kind: SyntaxKind) {
        // The root keeps leading trivia; any other node leaves it to its
        // parent.
        if !self.stack.is_empty() {
            self.eat_trivia();
        }
        self.stack.push((kind, self.children.len()));
    }

    fn finish_node(&mut self) {
        if self.stack.len() == 1 {
            // Trailing trivia belongs to the root.
            self.eat_trivia();
        }
        let (kind, first_child) = self.stack.pop().expect("unbalanced Finish event");
        let node = GreenNode::from_drain(kind, self.children.drain(first_child..));
        self.children.push(GreenElement::Node(node));
    }

    fn eat_trivia(&mut self) {
        while self.raw_pos < self.raw_end && self.input.raw_kinds[self.raw_pos].is_trivia() {
            let kind = self.input.raw_kinds[self.raw_pos];
            self.push_raw_token(kind, 1);
        }
    }

    fn token(&mut self, kind: SyntaxKind, n_raw: usize) {
        self.eat_trivia();
        self.push_raw_token(kind, n_raw);
    }

    /// Pushes one tree token made of the next `n_raw` raw tokens, reporting
    /// any lexical errors they carry.
    fn push_raw_token(&mut self, kind: SyntaxKind, n_raw: usize) {
        let start = self.input.raw_starts[self.raw_pos] as usize;
        let end = self.input.raw_starts[self.raw_pos + n_raw] as usize;
        for raw in self.raw_pos..self.raw_pos + n_raw {
            let raw_start = self.input.raw_starts[raw];
            let raw_end = self.input.raw_starts[raw + 1];
            let text = &self.input.text[raw_start as usize..raw_end as usize];
            let lexer_kind = self.input.raw[raw].kind;
            let message = if kind == ERROR && is_closer(lexer_kind) {
                Some(format!("unexpected closing delimiter: `{text}`"))
            } else {
                lexer_error(lexer_kind, text, self.input.edition)
            };
            if let Some(message) = message {
                self.errors.push(SyntaxError {
                    range: TextRange::new(raw_start - self.base, raw_end - self.base),
                    message,
                });
            }
        }
        let text = &self.input.text[start..end];
        let token = self.cache.token(kind, text);
        self.children.push(GreenElement::Token(token));
        self.raw_pos += n_raw;
        self.last_token = TextRange::new(start as u32 - self.base, end as u32 - self.base);
    }

    fn float_piece(&mut self, piece: u8, last: bool) {
        if piece == 0 {
            self.eat_trivia();
        }
        let start = self.input.raw_starts[self.raw_pos] as usize;
        let end = self.input.raw_starts[self.raw_pos + 1] as usize;
        let text = &self.input.text[start..end];
        let dot = text.find('.').expect("a split float contains a dot");
        let (kind, piece_text) = match piece {
            0 => (INT_NUMBER, &text[..dot]),
            1 => (DOT, &text[dot..dot + 1]),
            _ => (INT_NUMBER, &text[dot + 1..]),
        };
        let token = self.cache.token(kind, piece_text);
        self.children.push(GreenElement::Token(token));
        if last {
            let range = TextRange::new(start as u32 - self.base, end as u32 - self.base);
            let lexer_kind = self.input.raw[self.raw_pos].kind;
            if let Some(message) = lexer_error(lexer_kind, text, self.input.edition) {
                self.errors.push(SyntaxError { range, message });
            }
            self.raw_pos += 1;
            self.last_token = range;
        }
    }

    fn error(&mut self, message: String, on_previous_token: bool) {
        let range = if on_previous_token {
            self.last_token
        } else {
            TextRange::empty(self.offset())
        };
        self.errors.push(SyntaxError { range, message });
    }

    fn splice(&mut self, built: Built, raw_end: usize) {
        self.eat_trivia();
        let shift = self.offset() as i64;
        self.errors
            .extend(built.errors.into_iter().map(|error| SyntaxError {
                range: error.range.shifted(shift),
                message: error.message,
            }));
        self.children.push(GreenElement::Node(built.node));
        self.raw_pos = raw_end;
    }
}
