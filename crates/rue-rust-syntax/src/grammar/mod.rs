//! The Rust grammar, as functions over [`Parser`].
//!
//! The node structure follows rust-analyzer's conventions closely (`FN`,
//! `PARAM_LIST`, `BLOCK_EXPR` containing `STMT_LIST`, and so on), so the tree
//! shapes will be familiar to anyone who has worked with that grammar.
//!
//! Every delimited construct is parsed through [`Parser::group`]. A few
//! group-shaped nodes are *reparse roots*: their grammar entry,
//! [`parse_group`], depends on nothing but the group's own tokens, so a group
//! can be parsed in isolation (in parallel, or after an edit inside it) and
//! produce exactly the subtree an inline parse would.

mod attributes;
mod expressions;
mod generics;
mod items;
mod params;
mod paths;
mod patterns;
mod types;

use crate::parser::{CompletedMarker, Marker, Parser};
use crate::{SyntaxKind, SyntaxKind::*, T, TokenSet};

/// Node kinds that can be parsed in isolation from their group's tokens.
pub(crate) const REPARSE_ROOTS: &[SyntaxKind] = &[
    STMT_LIST,
    ITEM_LIST,
    ASSOC_ITEM_LIST,
    EXTERN_ITEM_LIST,
    MATCH_ARM_LIST,
    TOKEN_TREE,
    RECORD_FIELD_LIST,
    VARIANT_LIST,
    RECORD_EXPR_FIELD_LIST,
];

pub(crate) fn source_file(p: &mut Parser<'_, '_>) {
    let m = p.start();
    attributes::inner_attrs(p);
    items::mod_contents(p);
    m.complete(p, SOURCE_FILE);
}

/// Parses one reparse root of `kind`, starting at its opening delimiter.
///
/// Each arm must be the only function that creates `kind` nodes, and it must
/// be called with default parser context (no expression restrictions). Both
/// hold for every arm: the restriction state is passed as an argument and
/// reset by every delimiter.
pub(crate) fn parse_group(p: &mut Parser<'_, '_>, kind: SyntaxKind) {
    match kind {
        STMT_LIST => expressions::stmt_list(p),
        ITEM_LIST => items::item_list(p),
        ASSOC_ITEM_LIST => items::assoc_item_list(p),
        EXTERN_ITEM_LIST => items::extern_item_list(p),
        MATCH_ARM_LIST => expressions::match_arm_list(p),
        TOKEN_TREE => token_tree(p),
        RECORD_FIELD_LIST => items::record_field_list(p),
        VARIANT_LIST => items::variant_list(p),
        RECORD_EXPR_FIELD_LIST => expressions::record_expr_field_list(p),
        _ => unreachable!("{kind:?} is not a reparse root"),
    }
}

/// A delimited token tree, as in a macro invocation or attribute.
pub(crate) fn token_tree(p: &mut Parser<'_, '_>) {
    assert!(p.current().closing_delimiter().is_some());
    let m = p.start();
    p.group(|p| {
        while !p.at_eof() {
            if p.current().closing_delimiter().is_some() {
                token_tree(p);
            } else {
                p.bump_any();
            }
        }
    });
    m.complete(p, TOKEN_TREE);
}

/// A token tree that may be deferred: the body of `macro_rules!` or of an
/// item-level macro call, which can be large.
fn token_tree_deferrable(p: &mut Parser<'_, '_>) {
    p.defer_or(TOKEN_TREE, token_tree);
}

const FIELD_NAME_FIRST: TokenSet =
    TokenSet::new(&[IDENT, T![self], T![Self], T![super], T![crate]]);

fn name(p: &mut Parser<'_, '_>) {
    name_r(p, TokenSet::EMPTY);
}

fn name_r(p: &mut Parser<'_, '_>, recovery: TokenSet) {
    if p.at(IDENT) {
        let m = p.start();
        p.bump(IDENT);
        m.complete(p, NAME);
    } else {
        p.err_recover("expected a name", recovery);
    }
}

fn name_ref(p: &mut Parser<'_, '_>) {
    if p.at(IDENT) {
        let m = p.start();
        p.bump(IDENT);
        m.complete(p, NAME_REF);
    } else {
        p.err_and_bump("expected an identifier");
    }
}

/// A field name: an identifier or a tuple index. rustc's parser also takes
/// the path keywords (`self`, `Self`, `super`, `crate`), which later phases
/// reject.
fn name_ref_or_index(p: &mut Parser<'_, '_>) {
    assert!(p.at(INT_NUMBER) || p.at_ts(FIELD_NAME_FIRST));
    let suffixed = p.at(INT_NUMBER) && expressions::has_suffix(p);
    let m = p.start();
    p.bump_any();
    m.complete(p, NAME_REF);
    if suffixed {
        p.error_on_previous_token("suffixes on a tuple index are invalid");
    }
}

fn lifetime(p: &mut Parser<'_, '_>) {
    // Checked here rather than in the lexer: inside a macro's token tree,
    // `'pub` is fine.
    let keyword = !p.current_text().starts_with("'r#")
        && SyntaxKind::from_keyword(&p.current_text()[1..], p.edition())
            .is_some_and(|kw| kw != T![static]);
    let m = p.start();
    p.bump(LIFETIME_IDENT);
    m.complete(p, LIFETIME);
    if keyword {
        p.error_on_previous_token("lifetimes cannot use keyword names");
    }
}

fn opt_rename(p: &mut Parser<'_, '_>) {
    if p.at(T![as]) {
        let m = p.start();
        p.bump(T![as]);
        if !p.eat(T![_]) {
            name(p);
        }
        m.complete(p, RENAME);
    }
}

fn abi(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![extern]);
    if p.at(STRING) {
        p.bump(STRING);
    }
    m.complete(p, ABI);
}

fn opt_ret_type(p: &mut Parser<'_, '_>) -> bool {
    if !p.at(T![->]) {
        return false;
    }
    let m = p.start();
    p.bump(T![->]);
    types::type_no_bounds(p);
    m.complete(p, RET_TYPE);
    true
}

/// `pub`, `pub(crate)`, `pub(self)`, `pub(super)`, `pub(in path)`.
fn opt_visibility(p: &mut Parser<'_, '_>) -> bool {
    if !p.at(T![pub]) {
        return false;
    }
    let m = p.start();
    p.bump(T![pub]);
    // `pub (crate)` restricts visibility; `pub (u8)` in a tuple struct is a
    // public field of parenthesized type. Only the former has these shapes.
    if p.at(T!['(']) {
        let restricted = match p.nth(1) {
            T![crate] | T![self] | T![super] => p.nth(2) == EOF || p.nth(2) == T![')'],
            T![in] => true,
            _ => false,
        };
        if restricted {
            p.group(|p| {
                if p.eat(T![in]) {
                    paths::use_path(p);
                } else {
                    p.bump_any();
                }
            });
        }
    }
    m.complete(p, VISIBILITY);
    true
}

/// Parses `element`s separated by commas up to the end of the current
/// group, recovering from missing commas and unusable tokens.
fn comma_separated(p: &mut Parser<'_, '_>, mut element: impl FnMut(&mut Parser<'_, '_>)) {
    while !p.at_eof() {
        let start = p.pos();
        element(p);
        if p.at_eof() || p.eat(T![,]) {
            continue;
        }
        if p.pos() == start {
            p.err_and_bump("unexpected token");
        } else {
            p.error("expected `,`");
        }
    }
}

/// Whether a completed marker is a block-like expression, which ends a
/// statement without a semicolon.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockLike {
    Block,
    NotBlock,
}

impl BlockLike {
    fn is_block(self) -> bool {
        self == BlockLike::Block
    }

    fn of(kind: SyntaxKind) -> BlockLike {
        match kind {
            BLOCK_EXPR | IF_EXPR | WHILE_EXPR | FOR_EXPR | LOOP_EXPR | MATCH_EXPR => {
                BlockLike::Block
            }
            _ => BlockLike::NotBlock,
        }
    }
}
