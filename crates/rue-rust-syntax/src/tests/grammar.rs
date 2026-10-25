use super::*;
use crate::SyntaxKind::*;

/// Valid Rust covering the grammar's surface. Each snippet must parse
/// without errors, losslessly, identically in every parallelism mode.
const VALID: &[&str] = &[
    // Items and visibility.
    "pub(crate) struct S<'a, T: ?Sized + 'a, const N: usize = 3> where T: Clone { pub x: &'a T, y: [u8; N] }",
    "struct Unit; struct Tuple(pub u8, pub(super) String); struct Paren(pub (u8));",
    "union U { a: u32, b: f32 }",
    "enum E<T> { A, B(T), C { x: u8 } = 3, #[cfg(x)] D }",
    "pub(in crate::a) fn f<T>(x: T) -> impl Iterator<Item = T> + Send where T: Copy { std::iter::once(x) }",
    "const fn f() {} async fn g() {} unsafe fn h() {} const unsafe fn i() {} async unsafe fn j() {}",
    "extern \"C\" fn k() {} unsafe extern \"C\" fn l(x: i32, ...) {}",
    "extern crate alloc; extern crate self as me; extern crate std as _;",
    "unsafe extern \"C\" { safe fn sqrt(x: f64) -> f64; pub unsafe static ERRNO: i32; fn printf(fmt: *const u8, ...); }",
    "mod a; mod b { fn inner() {} } pub mod c { #![allow(dead_code)] }",
    "use std::{collections::{HashMap, hash_map::Entry}, io::{self, Read as _}}; use a::*; use ::b::c; use {d, e::f};",
    "type A<T> = Vec<T>; type B: Clone where Self: Sized;",
    "const X: u8 = 1; const _: () = (); static mut Y: &str = \"y\"; static Z: [u8; 2] = [0; 2];",
    "trait T: Clone + 'static { type A: Copy; const C: u8 = 1; fn m(&self) -> Self::A; fn d(&mut self) {} }",
    "unsafe trait U {} auto trait V {} pub unsafe auto trait W {} trait Alias = Clone + Send;",
    "impl<T> Tr for S<T> where T: Tr { type A = u8; fn m(&self) -> u8 { 0 } }",
    "impl S { pub const fn new() -> Self { S } } unsafe impl Send for S {} impl !Sync for S {} impl ! {}",
    "impl<T: ?Sized> Deref for Box<T> {} impl <T> Trait for T {} impl <T as Tr>::Assoc {}",
    "default fn f() {} impl<T> X for T { default type A = u8; default fn f() {} }",
    "macro_rules! m { ($x:expr) => { $x }; () => {} } macro_rules! n ( () => () ); macro m($x: expr) { $x }",
    "foo!(); bar![]; baz! { struct S; }",
    "#[derive(Debug)] #[cfg_attr(test, allow(x))] #[doc = \"text\"] #[unsafe(no_mangle)] fn f() {}",
    "#![no_std] #![allow(clippy::all)] fn main() {}",
    // Types.
    "type T = (u8, (i8,), (), [u8], [u8; 4], &'a mut T, *const u8, *mut (), !, _);",
    "type F = fn(u8, name: u16) -> u32; type G = unsafe extern \"C\" fn(*const u8, ...); type H = for<'a> fn(&'a u8);",
    "type D = Box<dyn Fn(&str) -> Result<(), E> + Send + Sync + 'static>; type I = impl Trait + use<'a, T>;",
    "type Q = <T as Trait>::Assoc; type R = <T>::Assoc; type S = Vec<<T as Trait>::Assoc>;",
    "type P = Vec<Vec<Vec<u8>>>; type O = Option<HashMap<String, Vec<(u8, u8)>>>;",
    "type K = Foo<'a, T, 3, { N + 1 }, -1, true, Item = u8, Assoc: Bound, Gat<'a> = &'a u8>;",
    "type M = m!(u8); fn f(_: impl for<'a> Fn(&'a u8) -> &'a u8) {}",
    "fn f<F>() where F: ~const Fn() + ?Sized, for<'a> &'a F: Copy, [u8; 2]: Sized, 'a: 'b + 'c {}",
    // Patterns.
    "fn f() { let (a, ref b, mut c, ref mut d) = x; let [first, .., last] = arr; let S { x, y: ref z, .. } = s; }",
    "fn f() { let T(a, _, ..) = t; let &&x = r; let &mut y = m; let box b = bx; let (x,) = one; let (y) = paren; }",
    "fn f() { match x { 0 | 1 => {} 2..=5 => {} ..=9 => {} 10.. => {} -1 => {} 'a'..='z' => {} _ => {} } }",
    "fn f() { match x { Some(y @ 1..=3) | Some(y @ 7) if y > 0 => (), E::A { .. } => (), m!() => (), const { 1 } => () } }",
    "fn f() { match x { | A | B => {} C::D(..) => {} [a, b @ .., c] => {} (.., z) => {} <T as Tr>::C => {} } }",
    "fn f(mut x: u8, (a, b): (u8, u8), S { x }: S, _: u8) {}",
    // Expressions.
    "fn f() { a = b += c; x..y; ..; a..; ..=b; *p = !q - -r; &raw const x; &raw mut y; &&z; &mut w; }",
    "fn f() { x.0.1; x.0 .1; t.0; s.field.method::<u8>().await?.0; f(a, b,)(c)[d]; }",
    "fn f() { x as u8 as u16; -x as i32; !x as u8 + 1; a < b && c > d || e >= f && g <= h; 1 << 2 >> 3; }",
    "fn f() { if a { } else if b { } else { }; if let Some(x) = y && let Ok(z) = w && x > z { } }",
    "fn f() { while let Some(x) = it.next() {} for (i, x) in xs.iter().enumerate() {} 'outer: loop { break 'outer 1; } }",
    "fn f() { let v = 'blk: { if c { break 'blk 1; } 2 }; continue; return; return 1; }",
    "fn f() { let c = |x| x + 1; let d = move |x: u8| -> u8 { x }; let e = || (); let g = async move || {}; }",
    "fn f() { let h = for<'a> |x: &'a u8| x; let s = static || {}; let i = |&x, (a, b)| a; }",
    "fn f() { async { 1 }.await; async move {}; unsafe { 1 }; const { 2 }; try { x? }; }",
    "fn f() { S { x: 1, y, ..Default::default() }; T { 0: a, 1: b }; S::<u8> { x }; }",
    "fn f() { [1, 2, 3]; [0; 4]; []; (); (1,); (1, 2); (1); vec![1]; format!(\"{x}\"); m! { a } }",
    "fn f() { 1; 1.0; 1e5; 0x1f_u8; b'a'; 'c'; \"s\"; b\"b\"; c\"c\"; r#\"r\"#; true; false; }",
    "fn f() { _ = x; (a, _) = t; [x, ..] = arr; S { a, .. } = s; }",
    "fn f() { <T as Tr>::f(); <T>::g(); Vec::<u8>::new(); ::std::mem::drop(x); Self::new(); self.x; crate::f(); super::g(); }",
    "fn f() { match x { A => 1, B => { 2 } C => if x { 3 } else { 4 }, D => match y { _ => 5 }, } }",
    "fn f() { yield 1; become f(); do yeet 2; box 1; }",
    "fn f() { x.await; x.await?; f.0(); (x.f)(); }",
    "fn f() { gen { }; gen move { }; async gen { }; }",
    // Statements.
    "fn f() { let x; let y: u8; let z = 1; let Some(w) = o else { return; }; ;;; }",
    "fn f() { if c {} - 1; match x {} (1); { 1 } [0]; loop {}.f(); x }",
    "fn f() { #[attr] let x = 1; #[attr] f(); #[attr] { } fn inner() {} struct Local; impl Local {} }",
    "fn f() -> u8 { #![inner] 1 }",
    "fn f() { if x == S {} for x in S {} while S != x {} match S {} }",
    // Shapes rustc's parser accepts and later phases reject, kept so the
    // parser matches rustc's notion of syntax.
    "fn f() { match x { Err(!), _ => 1, x } match y { ! } Ok::(u8); a::(u8) -> u8; }",
    "type R = Result<(), Self: Error>; fn g() { 1foo; \"s\"suffix; x.self; x.crate(); }",
    "type A = ?Sized; type B = ~const Tr; type C = async Fn(); fn f() -> const u8 {} m!('pub 'struct);",
    "fn f() { let 1u8 = x; let -1x = y; match s { \"a\"b => () } }",
];

/// Snippets rustc's parser rejects, each with the error this parser reports.
const INVALID: &[(&str, &str)] = &[
    (
        "fn f() { let x = let y = 1; }",
        "expected expression, found `let` statement",
    ),
    (
        "fn f() { if (let x = y) {} }",
        "expected expression, found `let` statement",
    ),
    (
        "fn f() { a < b < c; }",
        "comparison operators cannot be chained",
    ),
    (
        "fn f() { a == b != c; }",
        "comparison operators cannot be chained",
    ),
    ("fn f() { m!(a ] b); }", "unexpected closing delimiter: `]`"),
    ("fn f() { x.0u8; }", "suffixes on a tuple index are invalid"),
    (
        "fn f() { x.0.1f32; }",
        "suffixes on a tuple index are invalid",
    ),
    ("fn f(&'mut self) {}", "lifetimes cannot use keyword names"),
    (
        "fn f() { 'fn: loop {} }",
        "lifetimes cannot use keyword names",
    ),
    ("fn f() { let 1x = y; }", "expected identifier, found `1x`"),
    ("fn f() { x.fn; }", "expected a field name or method call"),
    ("type T = 'a;", "expected type, found lifetime"),
    ("fn f() { match x { y, _ => 1 } }", "expected `=>`"),
    ("struct S { x::: u8 }", "expected `:`"),
    (
        "fn f() { 'ab'; }",
        "character literal may only contain one codepoint",
    ),
    ("fn f() { '\\q'; }", "unknown character escape"),
    (
        "fn f() { \"\\u{110000}\"; }",
        "invalid unicode character escape",
    ),
    ("fn f() { b\"\\u{41}\"; }", "unicode escape in byte string"),
    ("fn f() { b'é'; }", "non-ASCII character in byte literal"),
    (
        "fn f() { c\"\\0\"; }",
        "null characters in C string literals are not supported",
    ),
    ("fn f() { \"\\x80\"; }", "out of range hex escape"),
    ("fn f() { 0b102; }", "invalid digit for a base 2 literal"),
    (
        "fn f() { 0x1.5; }",
        "hexadecimal float literal is not supported",
    ),
    ("m!(\"\\q\");", "unknown character escape"),
];

#[test]
fn invalid_snippets_report_rustc_errors() {
    for (snippet, message) in INVALID {
        let parse = check_modes(snippet);
        assert!(
            parse.errors().iter().any(|error| error.message == *message),
            "expected {message:?} for {snippet:?}:\n{}",
            parse.debug_dump()
        );
    }
}

#[test]
fn escapes_and_line_endings_that_are_valid() {
    check_ok(
        "fn f() { '\\''; '\\u{1F600}'; \"a\\\n   b\"; b'\\xFF'; c\"\\u{1F600}\"; \"\\x7F\"; }",
    );
    // A CRLF line ending is not a bare carriage return.
    check_ok("/// doc\r\nfn f() { \"a\r\nb\"; r\"a\r\nb\"; }\r\n");
}

#[test]
fn valid_snippets_parse_without_errors() {
    for snippet in VALID {
        check_ok(snippet);
    }
}

#[test]
fn editions_change_keywords() {
    let parse = |text: &str, edition| {
        crate::parse(
            text,
            ParseOptions {
                edition,
                ..ParseOptions::default()
            },
        )
    };
    // `async`, `dyn` and `try` are identifiers in 2015.
    let source = "fn async() { let try = 1; let x: Box<dyn Trait> = dyn_value; }";
    assert!(parse(source, Edition::Edition2015).errors().is_empty());
    // Anonymous parameters are a 2015 form.
    let anonymous = "trait T { fn f(&self, u32, &str); fn g(x: u8, Vec<u8>); }";
    assert!(parse(anonymous, Edition::Edition2015).errors().is_empty());
    assert!(!parse(anonymous, Edition::Edition2018).errors().is_empty());
    assert!(!parse(source, Edition::Edition2018).errors().is_empty());
    // A reserved prefix is an error only from 2021.
    let prefix = "macro_rules! m { ($($t:tt)*) => {} } m!(z#y);";
    assert!(parse(prefix, Edition::Edition2018).errors().is_empty());
    assert!(!parse(prefix, Edition::Edition2021).errors().is_empty());
}

#[test]
fn binary_operator_precedence() {
    assert_eq!(
        expr("1 + 2 * 3"),
        "(BIN_EXPR (LITERAL 1) + (BIN_EXPR (LITERAL 2) * (LITERAL 3)))"
    );
    assert_eq!(
        expr("1 * 2 + 3"),
        "(BIN_EXPR (BIN_EXPR (LITERAL 1) * (LITERAL 2)) + (LITERAL 3))"
    );
    assert_eq!(
        expr("a || b && c"),
        "(BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a)))) || \
         (BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF b)))) && (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF c))))))"
    );
    assert_eq!(
        expr("1 - 2 - 3"),
        "(BIN_EXPR (BIN_EXPR (LITERAL 1) - (LITERAL 2)) - (LITERAL 3))"
    );
    assert_eq!(
        expr("1 << 2 + 3"),
        "(BIN_EXPR (LITERAL 1) << (BIN_EXPR (LITERAL 2) + (LITERAL 3)))"
    );
    // Unlike C, bitwise operators bind tighter than comparisons.
    assert_eq!(
        expr("1 & 2 == 3"),
        "(BIN_EXPR (BIN_EXPR (LITERAL 1) & (LITERAL 2)) == (LITERAL 3))"
    );
}

#[test]
fn assignment_is_right_associative() {
    let source = "fn f() { a = b = c; }";
    assert_eq!(
        first(source, BIN_EXPR),
        "(BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a)))) = \
         (BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF b)))) = (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF c))))))"
    );
}

#[test]
fn prefix_postfix_and_cast_binding() {
    // Prefix operators bind looser than postfix ones and tighter than `as`.
    assert_eq!(
        expr("-a.b()"),
        "(PREFIX_EXPR - (METHOD_CALL_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a)))) . (NAME_REF b) (ARG_LIST ( ))))"
    );
    assert_eq!(
        expr("-1 as u8"),
        "(CAST_EXPR (PREFIX_EXPR - (LITERAL 1)) as (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF u8)))))"
    );
    // A cast's type does not absorb `+`.
    assert_eq!(
        expr("x as u8 + 1"),
        "(BIN_EXPR (CAST_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF x)))) as (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF u8))))) + (LITERAL 1))"
    );
    assert_eq!(
        expr("&mut *x?"),
        "(REF_EXPR & mut (PREFIX_EXPR * (TRY_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF x)))) ?)))"
    );
}

#[test]
fn ranges() {
    assert_eq!(
        expr("a..b + 1"),
        "(RANGE_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a)))) .. (BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF b)))) + (LITERAL 1)))"
    );
    assert_eq!(expr("..=5"), "(RANGE_EXPR ..= (LITERAL 5))");
    assert_eq!(expr("1.."), "(RANGE_EXPR (LITERAL 1) ..)");
    // `1..2` lexes as three tokens, not a float.
    assert_eq!(expr("1..2"), "(RANGE_EXPR (LITERAL 1) .. (LITERAL 2))");
}

#[test]
fn tuple_field_floats_are_split() {
    assert_eq!(
        expr("x.0.1"),
        "(FIELD_EXPR (FIELD_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF x)))) . (NAME_REF 0)) . (NAME_REF 1))"
    );
    // The split pieces keep the original text.
    let parse = check_ok("fn f() { x.0.1; }");
    let ints: Vec<_> = parse
        .syntax_node()
        .tokens()
        .filter(|t| t.kind() == INT_NUMBER)
        .map(|t| (t.text().to_string(), t.text_range()))
        .collect();
    assert_eq!(
        ints,
        vec![
            ("0".to_string(), TextRange::new(11, 12)),
            ("1".to_string(), TextRange::new(13, 14)),
        ]
    );
}

#[test]
fn closing_angle_brackets_are_glued_only_where_needed() {
    assert_eq!(
        first("type T = Vec<Vec<u8>>;", GENERIC_ARG_LIST),
        "(GENERIC_ARG_LIST < (TYPE_ARG (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF Vec) \
         (GENERIC_ARG_LIST < (TYPE_ARG (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF u8))))) >))))) >)"
    );
    assert_eq!(
        expr("a >> b"),
        "(BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a)))) >> (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF b)))))"
    );
    // `> >` with a space is not a shift.
    assert!(
        !crate::parse("fn f() { a > > b; }", options())
            .errors()
            .is_empty()
    );
}

#[test]
fn turbofish_and_comparison() {
    assert_eq!(
        expr("a < b"),
        "(BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a)))) < (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF b)))))"
    );
    assert_eq!(
        expr("f::<u8>()"),
        "(CALL_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF f) :: \
         (GENERIC_ARG_LIST < (TYPE_ARG (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF u8))))) >)))) (ARG_LIST ( )))"
    );
}

#[test]
fn no_struct_literal_in_conditions() {
    assert_eq!(
        first("fn f() { if x == S {} }", IF_EXPR),
        "(IF_EXPR if (BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF x)))) == \
         (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF S))))) (BLOCK_EXPR (STMT_LIST { })))"
    );
    // Parentheses lift the restriction.
    assert_eq!(
        first("fn f() { if x == (S {}) {} }", PAREN_EXPR),
        "(PAREN_EXPR ( (RECORD_EXPR (PATH (PATH_SEGMENT (NAME_REF S))) (RECORD_EXPR_FIELD_LIST { })) ))"
    );
}

#[test]
fn block_like_statements_end_without_semicolons() {
    let parse = check_ok("fn f() { match x {} - 1 }");
    let stmts = parse
        .syntax_node()
        .descendants()
        .find(|n| n.kind() == STMT_LIST)
        .unwrap();
    let kinds: Vec<_> = stmts.children().map(|n| n.kind()).collect();
    // The match is a statement; `-1` is the block's value.
    assert_eq!(kinds, vec![EXPR_STMT, PREFIX_EXPR]);
    // A method call continues a block-like expression.
    assert_eq!(
        first("fn f() { match x {}.len() }", METHOD_CALL_EXPR),
        "(METHOD_CALL_EXPR (MATCH_EXPR match (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF x)))) (MATCH_ARM_LIST { })) . (NAME_REF len) (ARG_LIST ( )))"
    );
}

#[test]
fn let_chains() {
    assert_eq!(
        first("fn f() { if let Some(x) = a && b {} }", BIN_EXPR),
        "(BIN_EXPR (LET_EXPR let (TUPLE_STRUCT_PAT (PATH (PATH_SEGMENT (NAME_REF Some))) ( (IDENT_PAT (NAME x)) )) = \
         (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF a))))) && (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF b)))))"
    );
}

#[test]
fn closures_extend_as_far_as_possible() {
    assert_eq!(
        expr("|x| x + 1"),
        "(CLOSURE_EXPR (PARAM_LIST | (PARAM (IDENT_PAT (NAME x))) |) \
         (BIN_EXPR (PATH_EXPR (PATH (PATH_SEGMENT (NAME_REF x)))) + (LITERAL 1)))"
    );
    assert_eq!(
        expr("|| ()"),
        "(CLOSURE_EXPR (PARAM_LIST ||) (TUPLE_EXPR ( )))"
    );
}

#[test]
fn attributes_attach_to_their_item() {
    assert_eq!(
        first("#[inline] pub fn f() {}", FN),
        "(FN (ATTR # [ (META (PATH (PATH_SEGMENT (NAME_REF inline)))) ]) (VISIBILITY pub) fn (NAME f) \
         (PARAM_LIST ( )) (BLOCK_EXPR (STMT_LIST { })))"
    );
}

#[test]
fn qualified_paths() {
    assert_eq!(
        first("type T = <A as B>::C;", PATH_TYPE),
        "(PATH_TYPE (PATH (PATH (PATH_SEGMENT < (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF A)))) as \
         (PATH_TYPE (PATH (PATH_SEGMENT (NAME_REF B)))) >)) :: (PATH_SEGMENT (NAME_REF C))))"
    );
}

#[test]
fn errors_are_reported_and_recovered() {
    let parse = check_modes("fn f() { g(1; let x = ; }\nfn h() {}\n");
    assert!(!parse.errors().is_empty());
    // The unclosed `(` ends at the block's closer, so recovery keeps the
    // following item intact.
    let fns = parse
        .syntax_node()
        .children()
        .filter(|n| n.kind() == FN)
        .count();
    assert_eq!(fns, 2, "{}", parse.debug_dump());

    let parse = check_modes("fn f() { a b }");
    assert_eq!(parse.errors()[0].message, "expected `;`");
    assert_eq!(parse.errors()[0].range, TextRange::empty(10));
}

#[test]
fn unclosed_and_stray_delimiters() {
    let parse = check_modes("fn f() { (1 }");
    assert!(
        parse
            .errors()
            .iter()
            .any(|e| e.message == "unclosed delimiter"),
        "{}",
        parse.debug_dump()
    );
    let parse = check_modes("fn f() { 1 ) }");
    assert!(!parse.errors().is_empty());
    // A stray closer becomes an error token; the block still closes.
    let block = parse
        .syntax_node()
        .descendants()
        .find(|n| n.kind() == STMT_LIST)
        .unwrap();
    assert_eq!(block.text(), "{ 1 ) }");
    let parse = check_modes("fn f() {");
    assert!(
        parse
            .errors()
            .iter()
            .any(|e| e.message == "unclosed delimiter")
    );
}

#[test]
fn lexical_errors_cover_their_token() {
    let parse = check_modes("fn f() { \"abc }");
    let error = &parse.errors()[0];
    assert_eq!(error.message, "unterminated string literal");
    assert_eq!(error.range, TextRange::new(9, 15));
}

#[test]
fn deferred_bodies_match_inline_parsing() {
    // Many bodies at several nesting levels, including error recovery inside
    // deferred groups.
    let mut source = String::new();
    for i in 0..50 {
        source.push_str(&format!(
            "/// doc\nimpl S{i} {{\n    fn a(&self) -> u8 {{ let x = {i}; x + 1 }}\n    fn b() {{ match y {{ _ => (), }} }}\n}}\n\
             mod m{i} {{ fn c() {{ broken( }} macro_rules! m {{ () => {{}} }} }}\n\
             trait T{i} {{ fn d(&self) {{ if a {{ b }} else {{ c }} }} }}\n"
        ));
    }
    check_modes(&source);
}
