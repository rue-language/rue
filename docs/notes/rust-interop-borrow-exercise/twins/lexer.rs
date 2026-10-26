// Twin of the core of examples/ruelex/lex.rue (a Rue lexer written in Rue).
//
// Two versions of the same lexer:
//
//   strict::   obeys R1-R3. The lexer cannot store `&str` (R3), so it OWNS the
//              source, and tokens are spans. This is the shape ruelex has
//              today (`Lexer.new(src: Bytes, ...)` takes ownership).
//   refined::  a borrowing type with one implicit lifetime: `Lexer<'a>` borrows
//              the source and yields `Token<'a>` whose text is a slice of it.
//              Not allowed by R3; allowed by the refined rule.
//
// Both are driven by `main` and must print identical output.

use std::io::Write;

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ident,
    Keyword,
    Int,
    Str,
    Op,
    Unknown,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Ident => "IDENT",
            Kind::Keyword => "KEYWORD",
            Kind::Int => "INT",
            Kind::Str => "STRING",
            Kind::Op => "OP",
            Kind::Unknown => "UNKNOWN",
        }
    }
}

const KEYWORDS: [&str; 12] = [
    "fn", "let", "mut", "if", "else", "while", "loop", "return", "struct", "enum", "match", "const",
];
const OPS2: [&str; 10] = ["==", "!=", "<=", ">=", "->", "=>", "&&", "||", "+=", "-="];

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// Shared scanning logic: given source bytes and a position, return the kind and
// end offset of the token starting at `start` (after trivia). Elision-shaped:
// the only borrow is an input.
fn scan_one(src: &[u8], start: usize) -> (Kind, usize) {
    let n = src.len();
    let b = src[start];
    if is_ident_start(b) {
        let mut i = start + 1;
        while i < n && is_ident_cont(src[i]) {
            i += 1;
        }
        let word = &src[start..i]; // R1: local borrow
        let kind = if KEYWORDS.iter().any(|k| k.as_bytes() == word) { Kind::Keyword } else { Kind::Ident };
        return (kind, i);
    }
    if b.is_ascii_digit() {
        let mut i = start + 1;
        while i < n && (src[i].is_ascii_digit() || src[i] == b'_') {
            i += 1;
        }
        return (Kind::Int, i);
    }
    if b == b'"' {
        let mut i = start + 1;
        while i < n && src[i] != b'"' {
            if src[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        return (Kind::Str, (i + 1).min(n));
    }
    if start + 1 < n {
        let two = &src[start..start + 2];
        if OPS2.iter().any(|op| op.as_bytes() == two) {
            return (Kind::Op, start + 2);
        }
    }
    if b"+-*/%=<>!&|(){}[];:,.".contains(&b) {
        return (Kind::Op, start + 1);
    }
    (Kind::Unknown, start + 1)
}

fn skip_trivia(src: &[u8], mut i: usize) -> usize {
    let n = src.len();
    loop {
        while i < n && src[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < n && src[i] == b'/' && src[i + 1] == b'/' {
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        return i;
    }
}

pub mod strict {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    pub struct Token {
        pub kind: Kind,
        pub start: usize,
        pub end: usize,
    }

    pub struct Lexer {
        src: String, // owned: R3 forbids `src: &str`
        pos: usize,
    }

    impl Lexer {
        pub fn new(src: String) -> Lexer {
            Lexer { src, pos: 0 }
        }
        // R2: output borrows from &self (elision)
        pub fn text(&self, t: Token) -> &str {
            &self.src[t.start..t.end]
        }
        pub fn into_source(self) -> String {
            self.src
        }
    }

    // An owning iterator: Item has no lifetime, so this impl names none.
    impl Iterator for Lexer {
        type Item = Token;
        fn next(&mut self) -> Option<Token> {
            let bytes = self.src.as_bytes(); // R1
            let start = skip_trivia(bytes, self.pos);
            if start >= bytes.len() {
                self.pos = start;
                return None;
            }
            let (kind, end) = scan_one(bytes, start);
            self.pos = end;
            Some(Token { kind, start, end })
        }
    }

    // Collect tokens, then read their text back through the lexer that owns
    // the source. Tokens and text never outlive `lx`.
    pub fn dump(src: String) -> String {
        let mut lx = Lexer::new(src);
        let mut toks: Vec<Token> = Vec::new();
        for t in lx.by_ref() {
            toks.push(t);
        }
        let mut s = String::new();
        for t in &toks {
            s.push_str(t.kind.name());
            s.push(' ');
            s.push_str(lx.text(*t)); // R1: borrow of lx held only for push_str
            s.push('\n');
        }
        s
    }
}

// refined: one implicit lifetime per borrowing type
pub mod refined {
    use super::*;

    pub struct Token<'a> {
        pub kind: Kind,
        pub text: &'a str,
    }

    pub struct Lexer<'a> {
        src: &'a str,
        pos: usize,
    }

    // Constructor: elision (single input borrow) would give this signature.
    pub fn lex(src: &str) -> Lexer<'_> {
        Lexer { src, pos: 0 }
    }

    impl<'a> Iterator for Lexer<'a> {
        type Item = Token<'a>;
        fn next(&mut self) -> Option<Token<'a>> {
            let bytes = self.src.as_bytes();
            let start = skip_trivia(bytes, self.pos);
            if start >= bytes.len() {
                self.pos = start;
                return None;
            }
            let (kind, end) = scan_one(bytes, start);
            self.pos = end;
            Some(Token { kind, text: &self.src[start..end] })
        }
    }

    pub fn dump(src: &str) -> String {
        let mut s = String::new();
        for t in lex(src) {
            s.push_str(t.kind.name());
            s.push(' ');
            s.push_str(t.text);
            s.push('\n');
        }
        s
    }
}
// end refined

fn main() {
    let src = "// sample\nfn main() -> i32 {\n    let mut x = 40;\n    x += 2; // answer\n    if x == 42 { return x; }\n    let s = \"hi \\\"there\\\"\";\n    0\n}\n";
    let a = strict::dump(src.to_string());
    let b = refined::dump(src);
    out(&a);
    if a != b {
        out("MISMATCH\n");
        std::process::exit(1);
    }
    let count = a.lines().count();
    out(&("tokens=".to_string() + &count.to_string() + "\n"));
}
