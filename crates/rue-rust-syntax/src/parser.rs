//! The event-producing parser engine the grammar is written against.
//!
//! The grammar never builds tree nodes directly. It emits a flat list of
//! [`Event`]s (start node, token, finish node, error) that the builder turns
//! into a green tree afterwards. Two properties of this engine carry the
//! frontend's parallel and incremental design:
//!
//! - **Groups bound lookahead.** Every `(...)`, `[...]` and `{...}` is parsed
//!   through [`Parser::group`], which hides everything past the group's
//!   closer. A group's parse therefore depends only on its own tokens.
//! - **Groups can be deferred.** Where a group is known to be a body (a
//!   function body, an `impl` block), the parser can record it as
//!   [`Event::Deferred`] and jump past it in O(1) using the precomputed
//!   delimiter matching. The driver parses deferred groups separately, in
//!   parallel, and splices the results in. Because a group's parse depends
//!   only on its tokens, the spliced tree is identical to an inline parse.

use crate::input::{Group, Input};
use crate::{SyntaxKind, SyntaxKind::*, T, TokenSet};
use std::cell::Cell;

pub(crate) enum Event {
    /// Starts a node. `forward_parent` links to a later `Start` that must
    /// enclose this one; it implements [`CompletedMarker::precede`].
    Start {
        kind: SyntaxKind,
        forward_parent: Option<u32>,
    },
    Finish,
    /// A token made of `n_raw` adjacent parser tokens, glued.
    Token {
        kind: SyntaxKind,
        n_raw: u8,
    },
    /// One piece of a float literal split for a tuple field access: `x.0.1`
    /// lexes `0.1` as one token but means `(x.0).1`. Piece 0 is the integer
    /// before the dot, 1 the dot, 2 the integer after it. The raw token is
    /// consumed with the last piece.
    FloatPiece {
        piece: u8,
        last: bool,
    },
    /// An error at the current position, or covering the token just
    /// consumed.
    Error {
        message: String,
        on_previous_token: bool,
    },
    /// A group parsed separately; `index` is its position in the parser's
    /// deferred list.
    Deferred {
        index: u32,
    },
}

/// A group whose parse was deferred.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeferredGroup {
    /// The node kind the group parses to, which names its grammar entry.
    pub(crate) kind: SyntaxKind,
    /// The parser index of the opener.
    pub(crate) start: usize,
    /// One past the parser index of the closer.
    pub(crate) end: usize,
}

/// The parser state over a window of an [`Input`].
pub(crate) struct Parser<'i, 't> {
    input: &'i Input<'t>,
    pos: usize,
    /// Lookahead stops here: the end of the innermost enclosing group.
    limit: usize,
    events: Vec<Event>,
    deferred: Vec<DeferredGroup>,
    /// Whether body groups may be deferred rather than parsed inline.
    defer: bool,
    /// Lookahead calls since the last token was consumed; a parser that looks
    /// without consuming forever has a grammar bug.
    steps: Cell<u32>,
}

const STEP_LIMIT: u32 = 10_000_000;

impl<'i, 't> Parser<'i, 't> {
    pub(crate) fn new(input: &'i Input<'t>, start: usize, end: usize, defer: bool) -> Self {
        Parser {
            input,
            pos: start,
            limit: end,
            events: Vec::new(),
            deferred: Vec::new(),
            defer,
            steps: Cell::new(0),
        }
    }

    pub(crate) fn finish(self) -> (Vec<Event>, Vec<DeferredGroup>) {
        (self.events, self.deferred)
    }

    pub(crate) fn edition(&self) -> crate::Edition {
        self.input.edition
    }

    /// The kind of the current token.
    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    /// The kind of the `n`th token from the current one, or `EOF` past the
    /// current group.
    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        assert!(n <= 4, "lookahead too far");
        let steps = self.steps.get() + 1;
        assert!(steps < STEP_LIMIT, "the parser seems stuck");
        self.steps.set(steps);
        let i = self.pos + n;
        if i < self.limit {
            self.input.kind(i)
        } else {
            EOF
        }
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.nth_at(0, kind)
    }

    /// Whether the `n`th token is `kind`, gluing composite punctuation.
    pub(crate) fn nth_at(&self, n: usize, kind: SyntaxKind) -> bool {
        match kind {
            T![-=] => self.at_composite2(n, T![-], T![=]),
            T![->] => self.at_composite2(n, T![-], T![>]),
            T![::] => self.at_composite2(n, T![:], T![:]),
            T![!=] => self.at_composite2(n, T![!], T![=]),
            T![..] => self.at_composite2(n, T![.], T![.]),
            T![*=] => self.at_composite2(n, T![*], T![=]),
            T![/=] => self.at_composite2(n, T![/], T![=]),
            T![&&] => self.at_composite2(n, T![&], T![&]),
            T![&=] => self.at_composite2(n, T![&], T![=]),
            T![%=] => self.at_composite2(n, T![%], T![=]),
            T![^=] => self.at_composite2(n, T![^], T![=]),
            T![+=] => self.at_composite2(n, T![+], T![=]),
            T![<<] => self.at_composite2(n, T![<], T![<]),
            T![<=] => self.at_composite2(n, T![<], T![=]),
            T![==] => self.at_composite2(n, T![=], T![=]),
            T![=>] => self.at_composite2(n, T![=], T![>]),
            T![>=] => self.at_composite2(n, T![>], T![=]),
            T![>>] => self.at_composite2(n, T![>], T![>]),
            T![|=] => self.at_composite2(n, T![|], T![=]),
            T![||] => self.at_composite2(n, T![|], T![|]),
            T![...] => self.at_composite3(n, T![.], T![.], T![.]),
            // rustc's lexer glues `::` greedily (`:::` is `::` then `:`) and
            // never splits it again, so a `:` that starts a `::` is not a `:`.
            T![:] => self.nth(n) == T![:] && !self.at_composite2(n, T![:], T![:]),
            T![..=] => self.at_composite3(n, T![.], T![.], T![=]),
            T![<<=] => self.at_composite3(n, T![<], T![<], T![=]),
            T![>>=] => self.at_composite3(n, T![>], T![>], T![=]),
            _ => self.nth(n) == kind,
        }
    }

    fn at_composite2(&self, n: usize, k1: SyntaxKind, k2: SyntaxKind) -> bool {
        self.nth(n) == k1 && self.nth(n + 1) == k2 && self.input.is_joint(self.pos + n)
    }

    fn at_composite3(&self, n: usize, k1: SyntaxKind, k2: SyntaxKind, k3: SyntaxKind) -> bool {
        self.nth(n) == k1
            && self.nth(n + 1) == k2
            && self.nth(n + 2) == k3
            && self.input.is_joint(self.pos + n)
            && self.input.is_joint(self.pos + n + 1)
    }

    pub(crate) fn at_ts(&self, set: TokenSet) -> bool {
        set.contains(self.current())
    }

    /// Whether the `n`th token is the identifier `kw` (a contextual keyword).
    pub(crate) fn nth_at_contextual_kw(&self, n: usize, kw: SyntaxKind) -> bool {
        self.nth(n) == IDENT && self.input.text(self.pos + n) == kw.static_text().unwrap()
    }

    pub(crate) fn at_contextual_kw(&self, kw: SyntaxKind) -> bool {
        self.nth_at_contextual_kw(0, kw)
    }

    /// Whether the current token is the identifier `text`.
    pub(crate) fn at_contextual_kw_text(&self, text: &str) -> bool {
        self.nth(0) == IDENT && self.input.text(self.pos) == text
    }

    /// Whether a token of `kind` was consumed at or after index `start`.
    pub(crate) fn consumed_since(&self, start: usize, kind: SyntaxKind) -> bool {
        (start..self.pos).any(|i| self.input.kind(i) == kind)
    }

    /// The index of the current token, for progress checks.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// The text of the current token. Used only for contextual decisions the
    /// token kinds cannot express.
    pub(crate) fn current_text(&self) -> &'t str {
        if self.pos < self.limit {
            self.input.text(self.pos)
        } else {
            ""
        }
    }

    /// The lexer's view of the current token: its literal kind and suffix.
    pub(crate) fn current_lexer_token(&self) -> Option<rue_rust_lexer::Token> {
        (self.pos < self.limit).then(|| self.input.lexer_token(self.pos))
    }

    pub(crate) fn at_eof(&self) -> bool {
        self.pos >= self.limit
    }

    /// Consumes `kind` if the parser is at it.
    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if !self.at(kind) {
            return false;
        }
        let n_raw = match kind {
            T![...] | T![..=] | T![<<=] | T![>>=] => 3,
            T![-=]
            | T![->]
            | T![::]
            | T![!=]
            | T![..]
            | T![*=]
            | T![/=]
            | T![&&]
            | T![&=]
            | T![%=]
            | T![^=]
            | T![+=]
            | T![<<]
            | T![<=]
            | T![==]
            | T![=>]
            | T![>=]
            | T![>>]
            | T![|=]
            | T![||] => 2,
            _ => 1,
        };
        self.do_bump(kind, n_raw);
        true
    }

    /// Consumes `kind`, which the parser must be at.
    pub(crate) fn bump(&mut self, kind: SyntaxKind) {
        assert!(
            self.eat(kind),
            "expected {kind:?}, found {:?}",
            self.current()
        );
    }

    /// Consumes the current token, whatever it is, with its own kind.
    pub(crate) fn bump_any(&mut self) {
        let kind = self.nth(0);
        if kind == EOF {
            return;
        }
        self.do_bump(kind, 1);
    }

    /// Consumes the current token as `kind` (a contextual keyword).
    pub(crate) fn bump_remap(&mut self, kind: SyntaxKind) {
        if self.nth(0) == EOF {
            return;
        }
        self.do_bump(kind, 1);
    }

    fn do_bump(&mut self, kind: SyntaxKind, n_raw: u8) {
        self.pos += n_raw as usize;
        self.steps.set(0);
        self.events.push(Event::Token { kind, n_raw });
    }

    /// Consumes a float literal as a tuple-field chain. Returns whether the
    /// literal ended in a dot (`0.`), in which case only the integer and dot
    /// pieces were emitted.
    pub(crate) fn split_float(&mut self, m: Marker) -> (bool, Marker) {
        let text = self.current_text();
        let ends_in_dot = text.ends_with('.');
        // `x.0.1`: NAME_REF(0) closes the first FIELD_EXPR; the dot and the
        // second integer belong to an enclosing one.
        let name_ref = self.start();
        self.events.push(Event::FloatPiece {
            piece: 0,
            last: false,
        });
        name_ref.complete(self, NAME_REF);
        let field = m.complete(self, FIELD_EXPR);
        let outer = field.precede(self);
        self.events.push(Event::FloatPiece {
            piece: 1,
            last: ends_in_dot,
        });
        if !ends_in_dot {
            let name_ref = self.start();
            self.events.push(Event::FloatPiece {
                piece: 2,
                last: true,
            });
            name_ref.complete(self, NAME_REF);
        }
        self.pos += 1;
        self.steps.set(0);
        (ends_in_dot, outer)
    }

    pub(crate) fn start(&mut self) -> Marker {
        let pos = self.events.len() as u32;
        self.events.push(Event::Start {
            kind: TOMBSTONE,
            forward_parent: None,
        });
        Marker { pos }
    }

    pub(crate) fn error(&mut self, message: impl Into<String>) {
        self.events.push(Event::Error {
            message: message.into(),
            on_previous_token: false,
        });
    }

    /// Reports an error covering the token just consumed.
    pub(crate) fn error_on_previous_token(&mut self, message: impl Into<String>) {
        self.events.push(Event::Error {
            message: message.into(),
            on_previous_token: true,
        });
    }

    /// Consumes `kind` or reports it missing.
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.eat(kind) {
            return true;
        }
        self.error(format!("expected {}", describe(kind)));
        false
    }

    /// Reports an error and consumes the current token into an `ERROR` node.
    pub(crate) fn err_and_bump(&mut self, message: &str) {
        let m = self.start();
        self.error(message);
        if self.current().closing_delimiter().is_some() {
            // Never split a group: consume it whole.
            self.skip_group_as_tokens();
        } else {
            self.bump_any();
        }
        m.complete(self, ERROR);
    }

    /// Reports an error, consuming the current token unless it is in
    /// `recovery` (a token the caller's caller can use) or past the group.
    pub(crate) fn err_recover(&mut self, message: &str, recovery: TokenSet) {
        if self.at_eof() || self.at_ts(recovery) {
            self.error(message);
            return;
        }
        self.err_and_bump(message);
    }

    /// The group opened by the current token.
    fn current_group(&self) -> Option<Group> {
        if self.pos < self.limit {
            self.input.group(self.pos)
        } else {
            None
        }
    }

    /// Parses the group opened by the current token: consumes the opener,
    /// runs `f` with lookahead limited to the group's interior, reports and
    /// consumes anything `f` left over, then consumes the closer.
    pub(crate) fn group(&mut self, f: impl FnOnce(&mut Self)) {
        let group = self
            .current_group()
            .expect("`group` called on a token that does not open a group");
        let saved_limit = self.limit;
        self.bump_any();
        self.limit = group.end;
        f(self);
        if !self.at_eof() {
            let m = self.start();
            self.error("unexpected tokens");
            while !self.at_eof() {
                self.bump_any();
            }
            m.complete(self, ERROR);
        }
        self.limit = saved_limit;
        if group.closed {
            self.bump_any();
        } else {
            self.error("unclosed delimiter");
        }
    }

    /// Consumes the group opened by the current token without interpreting
    /// it, as nested token trees.
    fn skip_group_as_tokens(&mut self) {
        let m = self.start();
        self.group(|p| {
            while !p.at_eof() {
                if p.current().closing_delimiter().is_some() {
                    p.skip_group_as_tokens();
                } else {
                    p.bump_any();
                }
            }
        });
        m.complete(self, TOKEN_TREE);
    }

    /// Parses the group at the current token with `f`, or, when deferral is
    /// enabled and the group is closed, records it for separate parsing as a
    /// `kind` node. `f` must parse exactly one `kind` node covering the group
    /// and must be the grammar entry for `kind` (see
    /// [`crate::grammar::parse_group`]); that equivalence is what makes
    /// deferral invisible in the resulting tree.
    pub(crate) fn defer_or(&mut self, kind: SyntaxKind, f: impl FnOnce(&mut Self)) {
        if self.defer {
            if let Some(group) = self.current_group() {
                if group.closed {
                    let index = self.deferred.len() as u32;
                    self.deferred.push(DeferredGroup {
                        kind,
                        start: self.pos,
                        end: group.end + 1,
                    });
                    self.events.push(Event::Deferred { index });
                    self.pos = group.end + 1;
                    self.steps.set(0);
                    return;
                }
            }
        }
        f(self)
    }
}

/// A started node, completed or abandoned by the grammar.
pub(crate) struct Marker {
    pos: u32,
}

impl Marker {
    pub(crate) fn complete(self, p: &mut Parser<'_, '_>, kind: SyntaxKind) -> CompletedMarker {
        match &mut p.events[self.pos as usize] {
            Event::Start { kind: slot, .. } => *slot = kind,
            _ => unreachable!("a marker always points at a Start event"),
        }
        p.events.push(Event::Finish);
        CompletedMarker {
            pos: self.pos,
            kind,
        }
    }

    /// Drops the node; its children become children of the enclosing node.
    pub(crate) fn abandon(self, p: &mut Parser<'_, '_>) {
        if self.pos as usize == p.events.len() - 1 {
            // Nothing was emitted inside: remove the start event outright.
            match p.events.pop() {
                Some(Event::Start {
                    kind: TOMBSTONE,
                    forward_parent: None,
                }) => {}
                _ => unreachable!("an abandoned marker is a fresh Start event"),
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CompletedMarker {
    pos: u32,
    kind: SyntaxKind,
}

impl CompletedMarker {
    /// Starts a node that will enclose this completed one, as when `a` turns
    /// out to be the left operand of `a + b`.
    pub(crate) fn precede(self, p: &mut Parser<'_, '_>) -> Marker {
        let new = p.start();
        match &mut p.events[self.pos as usize] {
            Event::Start { forward_parent, .. } => {
                *forward_parent = Some(new.pos - self.pos);
            }
            _ => unreachable!("a marker always points at a Start event"),
        }
        new
    }

    /// Moves the start of this node back to `m`, which was started before it
    /// and is abandoned: used to put leading attributes inside the node they
    /// turn out to belong to.
    pub(crate) fn extend_to(self, p: &mut Parser<'_, '_>, m: Marker) -> CompletedMarker {
        // `m` stays a tombstone whose forward parent is this node, so the
        // builder starts this node at `m`'s position.
        match &mut p.events[m.pos as usize] {
            Event::Start { forward_parent, .. } => {
                *forward_parent = Some(self.pos - m.pos);
            }
            _ => unreachable!("a marker always points at a Start event"),
        }
        self
    }

    pub(crate) fn kind(self) -> SyntaxKind {
        self.kind
    }
}

/// A human-readable name for a token kind in "expected X" messages.
fn describe(kind: SyntaxKind) -> String {
    match kind.static_text() {
        Some(text) => format!("`{text}`"),
        None => match kind {
            IDENT => "an identifier".to_string(),
            LIFETIME_IDENT => "a lifetime".to_string(),
            _ => format!("{kind:?}"),
        },
    }
}
