mod grammar;
mod incremental;

use crate::*;

fn options() -> ParseOptions {
    ParseOptions::default()
}

/// Parses `text`, asserting losslessness, agreement between all
/// parallelism modes, and the absence of errors.
fn check_ok(text: &str) -> Parse {
    let parse = check_modes(text);
    assert!(
        parse.errors().is_empty(),
        "unexpected errors in {text:?}:\n{}",
        parse.debug_dump()
    );
    parse
}

/// Parses `text`, asserting losslessness and agreement between modes.
fn check_modes(text: &str) -> Parse {
    let parse = |parallelism| {
        crate::parse(
            text,
            ParseOptions {
                parallelism,
                ..options()
            },
        )
    };
    let inline = parse(Parallelism::Inline);
    assert_eq!(inline.green().text(), text, "the tree must be lossless");
    assert_eq!(
        parse(Parallelism::Deferred),
        inline,
        "deferral changed the tree"
    );
    assert_eq!(
        parse(Parallelism::Parallel),
        inline,
        "parallelism changed the tree"
    );
    inline
}

/// A compact rendering of a tree without trivia: nodes as `(KIND ...)` and
/// tokens as their text.
fn sexp(node: &SyntaxNode) -> String {
    let mut out = format!("({:?}", node.kind());
    for child in node.children_with_tokens() {
        match child {
            SyntaxElement::Node(child) => {
                out.push(' ');
                out.push_str(&sexp(&child));
            }
            SyntaxElement::Token(token) if !token.kind().is_trivia() => {
                out.push(' ');
                out.push_str(token.text());
            }
            SyntaxElement::Token(_) => {}
        }
    }
    out.push(')');
    out
}

/// The s-expression of the first node of `kind` in `text`'s tree.
fn first(text: &str, kind: SyntaxKind) -> String {
    let parse = check_ok(text);
    let node = parse
        .syntax_node()
        .descendants()
        .find(|node| node.kind() == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in:\n{}", parse.debug_dump()));
    sexp(&node)
}

/// The s-expression of the initializer in `fn f() { let _ = EXPR; }`.
fn expr(text: &str) -> String {
    let source = format!("fn f() {{ let _ = {text}; }}");
    let parse = check_ok(&source);
    let let_stmt = parse
        .syntax_node()
        .descendants()
        .find(|node| node.kind() == SyntaxKind::LET_STMT)
        .unwrap();
    let init = let_stmt
        .children()
        .last()
        .expect("the initializer is the last node of the let statement");
    sexp(&init)
}

#[test]
fn debug_dump_format() {
    let parse = check_ok("fn f() {}\n");
    assert_eq!(
        parse.debug_dump(),
        "\
SOURCE_FILE@0..10
  FN@0..9
    FN_KW@0..2 \"fn\"
    WHITESPACE@2..3 \" \"
    NAME@3..4
      IDENT@3..4 \"f\"
    PARAM_LIST@4..6
      L_PAREN@4..5 \"(\"
      R_PAREN@5..6 \")\"
    WHITESPACE@6..7 \" \"
    BLOCK_EXPR@7..9
      STMT_LIST@7..9
        L_CURLY@7..8 \"{\"
        R_CURLY@8..9 \"}\"
  WHITESPACE@9..10 \"\\n\"
"
    );
}

#[test]
fn empty_and_trivia_only_files() {
    check_ok("");
    check_ok("  \n// comment\n/* block */\n");
    check_ok("#!/usr/bin/env run-cargo-script\nfn main() {}\n");
}
