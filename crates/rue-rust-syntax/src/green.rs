//! The immutable, position-independent "green" syntax tree.
//!
//! A green node stores its kind, its children and its total text length, but
//! no offset and no parent. That makes every subtree a value: it can be built
//! on any thread, shared between trees with `Arc`, and reused unchanged when an
//! edit elsewhere in the file shifts its position. The incremental reparser
//! replaces one subtree and rebuilds only the spine above it; every untouched
//! sibling is shared, by pointer, with the previous tree.
//!
//! Offsets and parent links live in the [`crate::SyntaxNode`] view built on
//! top of this tree.

use crate::SyntaxKind;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// An interior node of the green tree.
///
/// The kind and length live in the handle and the children in one shared
/// allocation, so building a node costs a single allocation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GreenNode {
    kind: SyntaxKind,
    text_len: u32,
    children: Arc<[GreenElement]>,
}

/// A leaf of the green tree: a token and its text.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GreenToken {
    kind: SyntaxKind,
    text: Arc<str>,
}

/// A child of a green node.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum GreenElement {
    Node(GreenNode),
    Token(GreenToken),
}

impl GreenNode {
    pub fn new(kind: SyntaxKind, children: Vec<GreenElement>) -> GreenNode {
        let text_len = children.iter().map(GreenElement::text_len).sum();
        GreenNode {
            kind,
            text_len,
            children: children.into(),
        }
    }

    /// Builds a node from the tail of a builder's child stack, allocating
    /// exactly once.
    pub(crate) fn from_drain(
        kind: SyntaxKind,
        children: std::vec::Drain<'_, GreenElement>,
    ) -> GreenNode {
        let text_len = children.as_slice().iter().map(GreenElement::text_len).sum();
        GreenNode {
            kind,
            text_len,
            children: children.collect(),
        }
    }

    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub fn text_len(&self) -> u32 {
        self.text_len
    }

    pub fn children(&self) -> &[GreenElement] {
        &self.children
    }

    /// Whether `self` and `other` are the same allocation, not merely equal.
    /// Subtrees an incremental reparse did not touch satisfy this.
    pub fn ptr_eq(&self, other: &GreenNode) -> bool {
        Arc::ptr_eq(&self.children, &other.children)
    }

    /// A copy of this node with child `index` replaced.
    pub fn replace_child(&self, index: usize, child: GreenElement) -> GreenNode {
        let mut children = self.children.to_vec();
        children[index] = child;
        GreenNode::new(self.kind(), children)
    }

    /// Appends the source text of this subtree to `out`.
    pub fn write_text(&self, out: &mut String) {
        for child in self.children() {
            match child {
                GreenElement::Node(node) => node.write_text(out),
                GreenElement::Token(token) => out.push_str(token.text()),
            }
        }
    }

    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.text_len() as usize);
        self.write_text(&mut out);
        out
    }
}

impl GreenToken {
    pub fn new(kind: SyntaxKind, text: &str) -> GreenToken {
        GreenToken {
            kind,
            text: text.into(),
        }
    }

    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn text_len(&self) -> u32 {
        self.text.len() as u32
    }
}

impl GreenElement {
    pub fn kind(&self) -> SyntaxKind {
        match self {
            GreenElement::Node(node) => node.kind(),
            GreenElement::Token(token) => token.kind(),
        }
    }

    pub fn text_len(&self) -> u32 {
        match self {
            GreenElement::Node(node) => node.text_len(),
            GreenElement::Token(token) => token.text_len(),
        }
    }
}

impl fmt::Debug for GreenNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{}", self.kind(), self.text_len())
    }
}

impl fmt::Debug for GreenToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} {:?}", self.kind(), self.text())
    }
}

impl fmt::Debug for GreenElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GreenElement::Node(node) => node.fmt(f),
            GreenElement::Token(token) => token.fmt(f),
        }
    }
}

/// Deduplicates tokens while building a tree.
///
/// Most tokens in a file are repeats (`(`, ` `, `self`, `let`), so sharing one
/// allocation per distinct `(kind, text)` pair keeps trees compact and cheap
/// to build. Tokens whose kind fixes their text (punctuation, keywords) are
/// found by kind alone; the rest go through a hash map. A cache is used by
/// one builder at a time; parallel builders each have their own, so building
/// never contends on a lock.
#[derive(Default)]
pub(crate) struct TokenCache {
    fixed: Vec<Option<GreenToken>>,
    tokens: HashMap<(SyntaxKind, Box<str>), GreenToken, ahash::RandomState>,
}

/// The hashed part of a cache is cleared when it grows past this many
/// entries, bounding a long-lived thread's memory.
const MAX_CACHED_TOKENS: usize = 1 << 16;

/// Tokens longer than this are allocated directly: long tokens (doc comments,
/// string literals) rarely repeat, and hashing them is wasted work.
const MAX_CACHED_TOKEN_LEN: usize = 16;

impl TokenCache {
    pub(crate) fn trim(&mut self) {
        if self.tokens.len() > MAX_CACHED_TOKENS {
            self.tokens.clear();
        }
    }

    pub(crate) fn token(&mut self, kind: SyntaxKind, text: &str) -> GreenToken {
        if kind.static_text() == Some(text) {
            let index = kind as usize;
            if self.fixed.len() <= index {
                self.fixed.resize(index + 1, None);
            }
            return self.fixed[index]
                .get_or_insert_with(|| GreenToken::new(kind, text))
                .clone();
        }
        if text.len() > MAX_CACHED_TOKEN_LEN {
            return GreenToken::new(kind, text);
        }
        // Look up by borrowed key first so a hit allocates nothing.
        if let Some(token) = self.tokens.get(&(kind, text) as &dyn CacheKey) {
            return token.clone();
        }
        let token = GreenToken::new(kind, text);
        self.tokens.insert((kind, text.into()), token.clone());
        token
    }
}
/// Lets the cache be probed with a borrowed `(SyntaxKind, &str)` key.
trait CacheKey {
    fn key(&self) -> (SyntaxKind, &str);
}

impl CacheKey for (SyntaxKind, Box<str>) {
    fn key(&self) -> (SyntaxKind, &str) {
        (self.0, &self.1)
    }
}

impl CacheKey for (SyntaxKind, &str) {
    fn key(&self) -> (SyntaxKind, &str) {
        (self.0, self.1)
    }
}

impl<'a> std::borrow::Borrow<dyn CacheKey + 'a> for (SyntaxKind, Box<str>) {
    fn borrow(&self) -> &(dyn CacheKey + 'a) {
        self
    }
}

impl PartialEq for dyn CacheKey + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for dyn CacheKey + '_ {}

impl std::hash::Hash for dyn CacheKey + '_ {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state)
    }
}
