// Twin of examples/jsonfmt (validate and compact a JSON document). Rust's std
// has no JSON, so the parser is written here, which is the interesting part:
//
//   strict::   R3 forbids `struct Parser<'a> { src: &'a [u8], .. }`, so parsing
//              is free functions over an explicit `(src, &mut pos)` pair. The
//              tree owns its strings. `impl Display for Json` needs no lifetime.
//   refined::  zero-copy: `Parser<'a>` and `JsonRef<'a>` borrow the input.
//
// Also exercises the C4 shape "result borrows from one of two inputs":
//   strict:  `get(obj, key) -> Option<&Json>`, caller does `.unwrap_or(fallback)`
//   refined: `get_or<'a>(obj: &'a Json, fallback: &'a Json, key) -> &'a Json`
//            (a dependency on two parameters; spelled `-> borrow(obj, fallback)`).

use std::fmt;
use std::io::Write;

fn out(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

pub mod strict {
    use super::*;

    pub enum Json {
        Null,
        Bool(bool),
        Num(String), // keep the lexeme; compaction must not reformat numbers
        Str(String),
        Arr(Vec<Json>),
        Obj(Vec<(String, Json)>),
    }

    fn ws(s: &[u8], i: &mut usize) {
        while *i < s.len() && (s[*i] == b' ' || s[*i] == b'\n' || s[*i] == b'\t' || s[*i] == b'\r') {
            *i += 1;
        }
    }

    fn eat(s: &[u8], i: &mut usize, lit: &[u8]) -> bool {
        if s[*i..].starts_with(lit) {
            *i += lit.len();
            true
        } else {
            false
        }
    }

    fn string(s: &[u8], i: &mut usize) -> Option<String> {
        if s.get(*i) != Some(&b'"') {
            return None;
        }
        *i += 1;
        let start = *i;
        while *i < s.len() && s[*i] != b'"' {
            if s[*i] == b'\\' {
                *i += 1;
            }
            *i += 1;
        }
        let body = s.get(start..*i)?; // R1: borrow held in a local
        *i += 1;
        // Keep escapes verbatim; compaction re-emits them unchanged.
        Some(String::from_utf8(body.to_vec()).ok()?)
    }

    pub fn value(s: &[u8], i: &mut usize) -> Option<Json> {
        ws(s, i);
        let b = *s.get(*i)?;
        let v = match b {
            b'n' if eat(s, i, b"null") => Json::Null,
            b't' if eat(s, i, b"true") => Json::Bool(true),
            b'f' if eat(s, i, b"false") => Json::Bool(false),
            b'"' => Json::Str(string(s, i)?),
            b'[' => {
                *i += 1;
                let mut items = Vec::new();
                ws(s, i);
                if !eat(s, i, b"]") {
                    loop {
                        items.push(value(s, i)?);
                        ws(s, i);
                        if eat(s, i, b"]") {
                            break;
                        }
                        if !eat(s, i, b",") {
                            return None;
                        }
                    }
                }
                Json::Arr(items)
            }
            b'{' => {
                *i += 1;
                let mut fields = Vec::new();
                ws(s, i);
                if !eat(s, i, b"}") {
                    loop {
                        ws(s, i);
                        let k = string(s, i)?;
                        ws(s, i);
                        if !eat(s, i, b":") {
                            return None;
                        }
                        fields.push((k, value(s, i)?));
                        ws(s, i);
                        if eat(s, i, b"}") {
                            break;
                        }
                        if !eat(s, i, b",") {
                            return None;
                        }
                    }
                }
                Json::Obj(fields)
            }
            b'-' | b'0'..=b'9' => {
                let start = *i;
                *i += 1;
                while *i < s.len() && (s[*i].is_ascii_digit() || b".eE+-".contains(&s[*i])) {
                    *i += 1;
                }
                Json::Num(String::from_utf8(s[start..*i].to_vec()).ok()?)
            }
            _ => return None,
        };
        Some(v)
    }

    pub fn parse(src: &str) -> Option<Json> {
        let s = src.as_bytes();
        let mut i = 0;
        let v = value(s, &mut i)?;
        ws(s, &mut i);
        if i == s.len() { Some(v) } else { None }
    }

    // R2: elision through the single borrowed input... two borrowed inputs
    // here (obj, key) so elision needs &self: make it a method.
    impl Json {
        pub fn get(&self, key: &str) -> Option<&Json> {
            match self {
                Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }
    }

    // Implementing a std trait for a Rue type: no lifetime needed.
    impl fmt::Display for Json {
        fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
            match self {
                Json::Null => f.write_str("null"),
                Json::Bool(b) => f.write_str(if *b { "true" } else { "false" }),
                Json::Num(n) => f.write_str(n),
                Json::Str(s) => {
                    f.write_str("\"")?;
                    f.write_str(s)?;
                    f.write_str("\"")
                }
                Json::Arr(items) => {
                    f.write_str("[")?;
                    for (n, it) in items.iter().enumerate() {
                        if n > 0 {
                            f.write_str(",")?;
                        }
                        fmt::Display::fmt(it, f)?;
                    }
                    f.write_str("]")
                }
                Json::Obj(fields) => {
                    f.write_str("{")?;
                    for (n, (k, v)) in fields.iter().enumerate() {
                        if n > 0 {
                            f.write_str(",")?;
                        }
                        f.write_str("\"")?;
                        f.write_str(k)?;
                        f.write_str("\":")?;
                        fmt::Display::fmt(v, f)?;
                    }
                    f.write_str("}")
                }
            }
        }
    }
}

// refined: borrowing types and a two-parameter dependency
pub mod refined {
    pub enum JsonRef<'a> {
        Null,
        Bool(bool),
        Num(&'a str),
        Str(&'a str),
        Arr(Vec<JsonRef<'a>>),
        Obj(Vec<(&'a str, JsonRef<'a>)>),
    }

    pub struct Parser<'a> {
        s: &'a str,
        i: usize,
    }

    impl<'a> Parser<'a> {
        pub fn new(s: &'a str) -> Parser<'a> {
            Parser { s, i: 0 }
        }
        fn ws(&mut self) {
            let b = self.s.as_bytes();
            while self.i < b.len() && b" \n\t\r".contains(&b[self.i]) {
                self.i += 1;
            }
        }
        fn eat(&mut self, lit: &str) -> bool {
            if self.s[self.i..].starts_with(lit) {
                self.i += lit.len();
                true
            } else {
                false
            }
        }
        fn string(&mut self) -> Option<&'a str> {
            let b = self.s.as_bytes();
            if b.get(self.i) != Some(&b'"') {
                return None;
            }
            self.i += 1;
            let start = self.i;
            while self.i < b.len() && b[self.i] != b'"' {
                if b[self.i] == b'\\' {
                    self.i += 1;
                }
                self.i += 1;
            }
            let body = self.s.get(start..self.i)?;
            self.i += 1;
            Some(body)
        }
        pub fn value(&mut self) -> Option<JsonRef<'a>> {
            self.ws();
            let b = *self.s.as_bytes().get(self.i)?;
            Some(match b {
                b'n' if self.eat("null") => JsonRef::Null,
                b't' if self.eat("true") => JsonRef::Bool(true),
                b'f' if self.eat("false") => JsonRef::Bool(false),
                b'"' => JsonRef::Str(self.string()?),
                b'[' => {
                    self.i += 1;
                    let mut items = Vec::new();
                    self.ws();
                    if !self.eat("]") {
                        loop {
                            items.push(self.value()?);
                            self.ws();
                            if self.eat("]") {
                                break;
                            }
                            if !self.eat(",") {
                                return None;
                            }
                        }
                    }
                    JsonRef::Arr(items)
                }
                b'{' => {
                    self.i += 1;
                    let mut fields = Vec::new();
                    self.ws();
                    if !self.eat("}") {
                        loop {
                            self.ws();
                            let k = self.string()?;
                            self.ws();
                            if !self.eat(":") {
                                return None;
                            }
                            fields.push((k, self.value()?));
                            self.ws();
                            if self.eat("}") {
                                break;
                            }
                            if !self.eat(",") {
                                return None;
                            }
                        }
                    }
                    JsonRef::Obj(fields)
                }
                b'-' | b'0'..=b'9' => {
                    let start = self.i;
                    self.i += 1;
                    let bs = self.s.as_bytes();
                    while self.i < bs.len() && (bs[self.i].is_ascii_digit() || b".eE+-".contains(&bs[self.i])) {
                        self.i += 1;
                    }
                    JsonRef::Num(&self.s[start..self.i])
                }
                _ => return None,
            })
        }
    }

    // depends: result borrows from `obj` or `fallback` (two parameters)
    pub fn get_or<'a>(obj: &'a JsonRef<'a>, fallback: &'a JsonRef<'a>, key: &str) -> &'a JsonRef<'a> {
        if let JsonRef::Obj(fields) = obj {
            for (k, v) in fields {
                if *k == key {
                    return v;
                }
            }
        }
        fallback
    }

    pub fn count_strings(v: &JsonRef) -> usize {
        match v {
            JsonRef::Str(_) => 1,
            JsonRef::Arr(items) => items.iter().map(count_strings).sum(),
            JsonRef::Obj(fields) => fields.iter().map(|(_, v)| count_strings(v)).sum(),
            _ => 0,
        }
    }
}
// end refined

fn main() {
    let src = "{ \"name\": \"rue\", \"tags\": [\"lang\", \"compiler\"],\n  \"version\": 0.1, \"ok\": true, \"nested\": {\"x\": null, \"n\": -12e3} }";
    let v = match strict::parse(src) {
        Some(v) => v,
        None => {
            out("error: invalid JSON\n");
            std::process::exit(3);
        }
    };
    let compact = v.to_string(); // Display -> ToString, no macro at the call site
    out(&(compact.clone() + "\n"));

    let missing = strict::Json::Str("n/a".to_string());
    let name = v.get("name").unwrap_or(&missing); // R1: Option<&Json> local
    let other = v.get("absent").unwrap_or(&missing);
    out(&(name.to_string() + " " + &other.to_string() + "\n"));

    let mut p = refined::Parser::new(src);
    let r = p.value().unwrap();
    let fb = refined::JsonRef::Null;
    let tags = refined::get_or(&r, &fb, "tags");
    out(&("strings=".to_string() + &refined::count_strings(&r).to_string()
        + " tags_strings=" + &refined::count_strings(tags).to_string() + "\n"));
}
