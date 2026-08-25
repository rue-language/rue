+++
title = "Lexical Structure"
weight = 2
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Lexical Structure

This chapter describes the lexical structure of Rue programs, including tokens, comments, and whitespace.

{{ rule(id="2.0:1", cat="normative") }}

The text of a source file is decomposed into a sequence of tokens. Comments and whitespace may separate tokens but are not themselves tokens.

{{ rule(id="2.0:9", cat="informative") }}

This chapter specifies the *decomposition*, not a compiler component: the rules
define which programs are well-formed and what sequence of tokens the later
chapters' grammar consumes, and an implementation may perform tokenization in
any manner — or not as a distinct stage at all — provided every program is
accepted, rejected, and interpreted exactly as these rules require. (This
follows the framing used by the C, Go, Java, and Rust specifications, which
define lexical structure declaratively rather than as behavior of a "lexer".)

## Maximal Munch

{{ rule(id="2.0:2", cat="normative") }}

Tokenization follows the *maximal munch* (or *longest match*) principle: at each position in the source text, the next token is the longest sequence of characters that forms a valid token.

{{ rule(id="2.0:3", cat="informative") }}

This principle resolves ambiguity when multiple token patterns could match at a position. For example, `<=` forms a single `<=` token rather than `<` followed by `=`, and `&&` forms a single logical AND token rather than two `&` tokens.

{{ rule(id="2.0:4", cat="example") }}

```rue
fn main() -> i32 {
    let x = 1 << 2;   // << is a single left-shift token
    let y = x <= 10;  // <= is a single less-than-or-equal token
    if true && false { 0 } else { 1 }  // && is a single logical AND token
}
```

## Source Encoding

{{ rule(id="2.0:5", cat="normative") }}

Source text is encoded in UTF-8. Each source file is read as a sequence of
Unicode scalar values decoded from its UTF-8 bytes. A file whose bytes are not
valid UTF-8 is rejected; no tokens are produced from it.

{{ rule(id="2.0:6", cat="legality-rule") }}

A Unicode scalar value outside the ASCII range (`U+0080` and above) **MAY**
appear only within a comment or a string literal. Everywhere else — in
identifiers, keywords, numeric literals, operators, and delimiters — only ASCII
characters may appear, and a non-ASCII character encountered in such a position
is a lexical error (E0001). Identifiers are therefore limited to ASCII letters,
digits, and underscores (2.1), while the *contents* of a comment or string
literal may be any UTF-8 text.

{{ rule(id="2.0:7", cat="example") }}

```rue
fn main() -> i32 {
    // Non-ASCII text is fine in a comment: café résumé π
    let s = "héllo, 世界";   // and inside a string literal
    0
}
```

{{ rule(id="2.0:8", cat="normative") }}

A single byte-order mark (`U+FEFF`) at the very start of a source file is
ignored: it does not produce a token and does not participate in tokenization.
This accommodates editors that prepend a UTF-8 BOM. A `U+FEFF` in any other
position is not whitespace (2.3:1) and is a lexical error (E0001), like any
other character that cannot begin a token.
