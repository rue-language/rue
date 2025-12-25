+++
title = "Lexical Structure"
weight = 2
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Lexical Structure

This chapter describes the lexical structure of Rue programs, including tokens, comments, and whitespace.

{{ rule(id="2.0:1") }}

The lexer processes source text and produces a sequence of tokens. Comments and whitespace are handled but do not produce tokens.

## Maximal Munch

{{ rule(id="2.0:2", cat="normative") }}

The lexer uses the *maximal munch* (or *longest match*) principle: at each position in the source text, the lexer consumes the longest sequence of characters that forms a valid token.

{{ rule(id="2.0:3", cat="informative") }}

This principle resolves ambiguity when multiple token patterns could match at a position. For example, `<=` is lexed as a single `<=` token rather than `<` followed by `=`, and `&&` is lexed as a single logical AND token rather than two `&` tokens.

{{ rule(id="2.0:4", cat="example") }}

```rue
fn main() -> i32 {
    let x = 1 << 2;   // << is a single left-shift token
    let y = x <= 10;  // <= is a single less-than-or-equal token
    if true && false { 0 } else { 1 }  // && is a single logical AND token
}
```
