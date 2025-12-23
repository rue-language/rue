# Whitespace

r[2.3:1#normative]
Whitespace consists of spaces, tabs, and newlines.

```ebnf
whitespace = " " | "\t" | "\n" | "\r" ;
```

r[2.3:2#normative]
Whitespace is ignored between tokens except where it serves to separate tokens.

r[2.3:3#normative]
Multiple whitespace characters between tokens are equivalent to a single space.

r[2.3:4]
```rue
// Minimal whitespace
fn main()->i32{42}

// Generous whitespace
fn   main()   ->   i32   {   42   }

// Both programs are equivalent
```
