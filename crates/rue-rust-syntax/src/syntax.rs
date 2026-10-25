//! The "red" view of a green tree: nodes with offsets and parent links.
//!
//! Red nodes are created on demand while walking the tree and are cheap,
//! thread-local handles. They are what consumers navigate; the green tree is
//! what is stored, shared and reused.

use crate::{GreenElement, GreenNode, GreenToken, SyntaxKind};
use std::fmt;
use std::rc::Rc;

/// A half-open byte range `start..end` in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> TextRange {
        TextRange { start, end }
    }

    pub const fn empty(offset: u32) -> TextRange {
        TextRange::new(offset, offset)
    }

    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub const fn contains_range(self, other: TextRange) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub const fn shifted(self, by: i64) -> TextRange {
        TextRange::new(
            (self.start as i64 + by) as u32,
            (self.end as i64 + by) as u32,
        )
    }
}

impl fmt::Display for TextRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

/// A node in a syntax tree, with its absolute position.
#[derive(Clone)]
pub struct SyntaxNode(Rc<NodeData>);

struct NodeData {
    green: GreenNode,
    parent: Option<SyntaxNode>,
    offset: u32,
}

/// A token in a syntax tree, with its absolute position.
#[derive(Clone)]
pub struct SyntaxToken {
    green: GreenToken,
    parent: SyntaxNode,
    offset: u32,
}

/// A child of a syntax node.
#[derive(Clone, Debug)]
pub enum SyntaxElement {
    Node(SyntaxNode),
    Token(SyntaxToken),
}

impl SyntaxNode {
    pub fn new_root(green: GreenNode) -> SyntaxNode {
        SyntaxNode(Rc::new(NodeData {
            green,
            parent: None,
            offset: 0,
        }))
    }

    pub fn kind(&self) -> SyntaxKind {
        self.0.green.kind()
    }

    pub fn green(&self) -> &GreenNode {
        &self.0.green
    }

    pub fn parent(&self) -> Option<SyntaxNode> {
        self.0.parent.clone()
    }

    pub fn text_range(&self) -> TextRange {
        TextRange::new(self.0.offset, self.0.offset + self.0.green.text_len())
    }

    pub fn text(&self) -> String {
        self.0.green.text()
    }

    pub fn children_with_tokens(&self) -> impl Iterator<Item = SyntaxElement> + '_ {
        let mut offset = self.0.offset;
        self.0.green.children().iter().map(move |child| {
            let start = offset;
            offset += child.text_len();
            match child {
                GreenElement::Node(green) => SyntaxElement::Node(SyntaxNode(Rc::new(NodeData {
                    green: green.clone(),
                    parent: Some(self.clone()),
                    offset: start,
                }))),
                GreenElement::Token(green) => SyntaxElement::Token(SyntaxToken {
                    green: green.clone(),
                    parent: self.clone(),
                    offset: start,
                }),
            }
        })
    }

    pub fn children(&self) -> impl Iterator<Item = SyntaxNode> + '_ {
        self.children_with_tokens().filter_map(|child| match child {
            SyntaxElement::Node(node) => Some(node),
            SyntaxElement::Token(_) => None,
        })
    }

    /// This node and all nodes below it, in preorder.
    pub fn descendants(&self) -> impl Iterator<Item = SyntaxNode> {
        let mut stack = vec![self.clone()];
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            let mut children: Vec<_> = node.children().collect();
            children.reverse();
            stack.extend(children);
            Some(node)
        })
    }

    /// All tokens below this node, in source order.
    pub fn tokens(&self) -> impl Iterator<Item = SyntaxToken> {
        let mut stack = vec![SyntaxElement::Node(self.clone())];
        std::iter::from_fn(move || {
            loop {
                match stack.pop()? {
                    SyntaxElement::Token(token) => return Some(token),
                    SyntaxElement::Node(node) => {
                        let mut children: Vec<_> = node.children_with_tokens().collect();
                        children.reverse();
                        stack.extend(children);
                    }
                }
            }
        })
    }

    /// An indented outline of the tree, one node or token per line, as used
    /// by tests and debugging tools:
    ///
    /// ```text
    /// SOURCE_FILE@0..9
    ///   FN@0..9
    ///     FN_KW@0..2 "fn"
    /// ```
    pub fn debug_dump(&self) -> String {
        let mut out = String::new();
        dump(&SyntaxElement::Node(self.clone()), 0, &mut out);
        out
    }
}

fn dump(element: &SyntaxElement, depth: usize, out: &mut String) {
    use std::fmt::Write;
    for _ in 0..depth {
        out.push_str("  ");
    }
    match element {
        SyntaxElement::Node(node) => {
            let _ = writeln!(out, "{:?}@{}", node.kind(), node.text_range());
            for child in node.children_with_tokens() {
                dump(&child, depth + 1, out);
            }
        }
        SyntaxElement::Token(token) => {
            let _ = writeln!(
                out,
                "{:?}@{} {:?}",
                token.kind(),
                token.text_range(),
                token.text()
            );
        }
    }
}

impl SyntaxToken {
    pub fn kind(&self) -> SyntaxKind {
        self.green.kind()
    }

    pub fn text(&self) -> &str {
        self.green.text()
    }

    pub fn text_range(&self) -> TextRange {
        TextRange::new(self.offset, self.offset + self.green.text_len())
    }

    pub fn parent(&self) -> SyntaxNode {
        self.parent.clone()
    }
}

impl SyntaxElement {
    pub fn kind(&self) -> SyntaxKind {
        match self {
            SyntaxElement::Node(node) => node.kind(),
            SyntaxElement::Token(token) => token.kind(),
        }
    }

    pub fn text_range(&self) -> TextRange {
        match self {
            SyntaxElement::Node(node) => node.text_range(),
            SyntaxElement::Token(token) => token.text_range(),
        }
    }
}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{}", self.kind(), self.text_range())
    }
}

impl fmt::Debug for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}@{} {:?}",
            self.kind(),
            self.text_range(),
            self.text()
        )
    }
}

impl fmt::Display for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text())
    }
}
