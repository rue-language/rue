use crate::*;

/// Renders tokens as `Kind "text"` lines for compact assertions.
fn dump(input: &str) -> String {
    let mut out = String::new();
    let mut offset = 0usize;
    for token in tokenize(input) {
        let end = offset + token.len as usize;
        out.push_str(&format!("{:?} {:?}\n", token.kind, &input[offset..end]));
        offset = end;
    }
    assert_eq!(offset, input.len(), "token lengths must cover the input");
    out
}

fn kinds(input: &str) -> Vec<TokenKind> {
    tokenize(input).into_iter().map(|t| t.kind).collect()
}

fn texts(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for token in tokenize(input) {
        let end = offset + token.len as usize;
        if token.kind != TokenKind::Whitespace {
            out.push(&input[offset..end]);
        }
        offset = end;
    }
    out
}

fn lit(kind: LiteralKind, suffix_start: u32) -> TokenKind {
    TokenKind::Literal { kind, suffix_start }
}

#[test]
fn punctuation_is_single_characters() {
    assert_eq!(
        texts(">>= :: -> .."),
        vec![">", ">", "=", ":", ":", "-", ">", ".", "."]
    );
}

#[test]
fn identifiers_keywords_and_raw_identifiers() {
    assert_eq!(
        kinds("fn r#match _ ünïcödé"),
        vec![
            TokenKind::Ident,
            TokenKind::Whitespace,
            TokenKind::RawIdent,
            TokenKind::Whitespace,
            TokenKind::Ident,
            TokenKind::Whitespace,
            TokenKind::Ident,
        ]
    );
}

#[test]
fn comments_and_doc_styles() {
    let expected = "\
LineComment { doc_style: None } \"// plain\"
Whitespace \"\\n\"
LineComment { doc_style: Some(Outer) } \"/// outer\"
Whitespace \"\\n\"
LineComment { doc_style: Some(Inner) } \"//! inner\"
Whitespace \"\\n\"
LineComment { doc_style: None } \"//// not doc\"
Whitespace \"\\n\"
BlockComment { doc_style: Some(Outer), terminated: true } \"/** outer */\"
BlockComment { doc_style: None, terminated: true } \"/**/\"
BlockComment { doc_style: None, terminated: true } \"/*** no */\"
BlockComment { doc_style: Some(Inner), terminated: true } \"/*! in */\"
";
    assert_eq!(
        dump("// plain\n/// outer\n//! inner\n//// not doc\n/** outer *//**//*** no *//*! in */"),
        expected
    );
}

#[test]
fn nested_and_unterminated_block_comments() {
    assert_eq!(
        kinds("/* a /* b */ c */x"),
        vec![
            TokenKind::BlockComment {
                doc_style: None,
                terminated: true
            },
            TokenKind::Ident
        ]
    );
    assert_eq!(
        kinds("/* a /* b */"),
        vec![TokenKind::BlockComment {
            doc_style: None,
            terminated: false
        }]
    );
}

#[test]
fn numbers() {
    use LiteralKind::*;
    let int = |base| Int {
        base,
        empty_int: false,
    };
    let float = Float {
        base: Base::Decimal,
        empty_exponent: false,
    };
    assert_eq!(kinds("12"), vec![lit(int(Base::Decimal), 2)]);
    assert_eq!(kinds("0x1F_u8"), vec![lit(int(Base::Hexadecimal), 5)]);
    assert_eq!(kinds("0b1010"), vec![lit(int(Base::Binary), 6)]);
    assert_eq!(kinds("0o17i32"), vec![lit(int(Base::Octal), 4)]);
    assert_eq!(
        kinds("0x"),
        vec![lit(
            Int {
                base: Base::Hexadecimal,
                empty_int: true
            },
            2
        )]
    );
    assert_eq!(kinds("1.5"), vec![lit(float, 3)]);
    assert_eq!(kinds("1.5e-3f64"), vec![lit(float, 6)]);
    assert_eq!(kinds("1."), vec![lit(float, 2)]);
    assert_eq!(kinds("1e10"), vec![lit(float, 4)]);
    assert_eq!(
        kinds("1e"),
        vec![lit(
            Float {
                base: Base::Decimal,
                empty_exponent: true
            },
            2
        )]
    );
}

#[test]
fn number_dot_disambiguation() {
    // A range, a method call and a tuple-field chain all keep the dot apart.
    assert_eq!(texts("1..2"), vec!["1", ".", ".", "2"]);
    assert_eq!(texts("1.foo()"), vec!["1", ".", "foo", "(", ")"]);
    // `x.0.1` lexes `0.1` as one float; the parser splits it.
    assert_eq!(texts("x.0.1"), vec!["x", ".", "0.1"]);
}

#[test]
fn chars_and_lifetimes() {
    use LiteralKind::*;
    assert_eq!(kinds("'a'"), vec![lit(Char { terminated: true }, 3)]);
    assert_eq!(kinds("'\\n'"), vec![lit(Char { terminated: true }, 4)]);
    assert_eq!(kinds("'\\''"), vec![lit(Char { terminated: true }, 4)]);
    assert_eq!(
        kinds("'a"),
        vec![TokenKind::Lifetime {
            starts_with_number: false
        }]
    );
    assert_eq!(
        kinds("'static"),
        vec![TokenKind::Lifetime {
            starts_with_number: false
        }]
    );
    assert_eq!(kinds("'r#a"), vec![TokenKind::RawLifetime]);
    assert_eq!(
        kinds("'1"),
        vec![TokenKind::Lifetime {
            starts_with_number: true
        }]
    );
    // An unterminated character literal stops at the newline.
    assert_eq!(texts("' x\ny"), vec!["' x", "y"]);
    // A label followed by a loop.
    assert_eq!(texts("'outer: loop"), vec!["'outer", ":", "loop"]);
}

#[test]
fn strings() {
    use LiteralKind::*;
    assert_eq!(kinds(r#""a\"b""#), vec![lit(Str { terminated: true }, 6)]);
    assert_eq!(kinds(r#""abc"#), vec![lit(Str { terminated: false }, 4)]);
    assert_eq!(
        kinds(r#"b"ab""#),
        vec![lit(ByteStr { terminated: true }, 5)]
    );
    assert_eq!(kinds(r#"c"ab""#), vec![lit(CStr { terminated: true }, 5)]);
    assert_eq!(kinds("b'a'"), vec![lit(Byte { terminated: true }, 4)]);
    assert_eq!(
        kinds(r###"r##"a"#b"##"###),
        vec![lit(RawStr { n_hashes: Some(2) }, 11)]
    );
    assert_eq!(
        kinds(r##"br#"x"#"##),
        vec![lit(RawByteStr { n_hashes: Some(1) }, 7)]
    );
    assert_eq!(
        kinds(r#"cr"x""#),
        vec![lit(RawCStr { n_hashes: Some(0) }, 5)]
    );
    assert_eq!(
        kinds(r#"r#"unterminated"#),
        vec![lit(RawStr { n_hashes: None }, 15)]
    );
    assert_eq!(
        kinds(r#""s"suffix"#),
        vec![lit(Str { terminated: true }, 3)]
    );
}

#[test]
fn prefixed_identifiers_are_not_strings() {
    assert_eq!(
        texts("b c r br cr bx"),
        vec!["b", "c", "r", "br", "cr", "bx"]
    );
    assert_eq!(
        kinds("foo\"x\""),
        vec![
            TokenKind::UnknownPrefix,
            lit(LiteralKind::Str { terminated: true }, 3)
        ]
    );
}

#[test]
fn shebang() {
    assert_eq!(
        kinds("#!/usr/bin/env rust\nfn"),
        vec![TokenKind::Shebang, TokenKind::Whitespace, TokenKind::Ident]
    );
    // An inner attribute is not a shebang, even across lines and comments.
    assert_eq!(strip_shebang("#![allow(x)]"), None);
    assert_eq!(strip_shebang("#! // c\n [allow(x)]"), None);
    assert_eq!(strip_shebang("#!"), Some(2));
    // `first_token` never recognizes a shebang.
    assert_eq!(first_token("#!x").unwrap().kind, TokenKind::Pound);
}

#[test]
fn unknown_characters() {
    assert_eq!(texts("a ¤ b"), vec!["a", "¤", "b"]);
    assert_eq!(kinds("¤")[0], TokenKind::Unknown);
}

/// Every token boundary is a resynchronization point: lexing the suffix that
/// starts at any boundary reproduces the tail of the whole-file token stream.
#[test]
fn lexing_from_any_boundary_matches_the_whole_file() {
    let input = r###"
fn main() {
    let s = r#"raw "string""#; let c = 'c'; 'label: loop { break 'label; }
    /* nested /* comment */ */ x.0.1 + 1.5e3 - 0x_ff..=10 b"bytes" c"c"
}
"###;
    let tokens = tokenize(input);
    let mut offset = 0usize;
    for (i, token) in tokens.iter().enumerate() {
        let mut tail = Vec::new();
        tokenize_from(&input[offset..], &mut tail);
        assert_eq!(
            &tail[..],
            &tokens[i..],
            "mismatch resuming at byte {offset}"
        );
        offset += token.len as usize;
    }
}

/// The lexer is lossless and makes progress on arbitrary input, including
/// truncations that cut through multi-byte characters' neighbours.
#[test]
fn lossless_on_every_prefix() {
    let input = "fn f<'a>(x: &'a str) -> r#\"q\"# { /* /* */ 'x' b'\\'' \"\\\"\" ünï 1.2e+3 }";
    for end in (0..=input.len()).filter(|&i| input.is_char_boundary(i)) {
        let prefix = &input[..end];
        let total: u32 = tokenize(prefix).iter().map(|t| t.len).sum();
        assert_eq!(total as usize, prefix.len());
    }
}
