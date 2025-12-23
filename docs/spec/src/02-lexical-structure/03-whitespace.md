+++
title = "Whitespace"
weight = 3
template = "spec/page.html"
+++

# Whitespace

{{ rule(id="2.3:1", cat="normative") }}

Whitespace consists of spaces, tabs, and newlines.

```ebnf
whitespace = " " | "\t" | "\n" | "\r" ;
```

{{ rule(id="2.3:2", cat="normative") }}

Whitespace is ignored between tokens except where it serves to separate tokens.

{{ rule(id="2.3:3", cat="normative") }}

Multiple whitespace characters between tokens are equivalent to a single space.

{{ rule(id="2.3:4") }}

```rue
// Minimal whitespace
fn main()->i32{42}

// Generous whitespace
fn   main()   ->   i32   {   42   }

// Both programs are equivalent
```
