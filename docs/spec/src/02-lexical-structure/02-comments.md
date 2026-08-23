+++
title = "Comments"
weight = 2
template = "spec/page.html"
+++

# Comments

{{ rule(id="2.2:1", cat="normative") }}

Line comments begin with `//` and extend to the end of the line.

<!-- grammar-sync(id="2.2:1", production="any_char_except_newline", role="source") -->
<!-- grammar-sync(id="2.2:1", production="newline", role="source") -->
<!-- grammar-sync(id="2.2:1", production="line_comment", role="source") -->

```ebnf
any_char_except_newline = ? any character except '\n' or '\r' ? ;
newline = "\r\n" | "\n" | "\r" ;
line_comment = "//" { any_char_except_newline } [ newline ] ;
```

{{ rule(id="2.2:2", cat="normative") }}

Comments are discarded during lexical analysis and do not affect program semantics.

{{ rule(id="2.2:3") }}

```rue
// This is a comment
fn main() -> i32 {
    42  // This is also a comment
}
```

{{ rule(id="2.2:4") }}

Block comments (`/* ... */`) are not currently supported.
