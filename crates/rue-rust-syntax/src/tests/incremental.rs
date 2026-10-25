use super::*;
use crate::SyntaxKind::*;

const SOURCE: &str = r#"
use std::collections::HashMap;

/// A counter.
pub struct Counter {
    counts: HashMap<String, usize>,
}

impl Counter {
    pub fn add(&mut self, word: &str) {
        let entry = self.counts.entry(word.to_string()).or_insert(0);
        *entry += 1;
    }

    pub fn get(&self, word: &str) -> usize {
        match self.counts.get(word) {
            Some(&n) => n,
            None => 0,
        }
    }
}

fn main() {
    let mut c = Counter { counts: HashMap::new() };
    for w in "a b a".split(' ') {
        c.add(w);
    }
    println!("{}", c.get("a"));
}
"#;

fn document() -> Document {
    Document::new(SOURCE, options())
}

fn offset_of(text: &str, needle: &str) -> u32 {
    text.find(needle).expect("needle in text") as u32
}

/// Applies an edit and checks the result against a fresh parse.
fn edit(document: &mut Document, delete: TextRange, insert: &str) -> Reparse {
    let reparse = document.edit(delete, insert);
    let fresh = crate::parse(document.text(), options());
    assert_eq!(
        document.parse(),
        &fresh,
        "incremental reparse diverged after replacing {delete} with {insert:?}:\n{}",
        document.text()
    );
    reparse
}

#[test]
fn an_edit_inside_a_body_reparses_only_that_body() {
    let mut document = document();
    let before = document.parse().green().clone();
    let at = offset_of(document.text(), "*entry += 1") + 9;
    let reparse = edit(&mut document, TextRange::new(at, at + 1), "2");
    match reparse {
        Reparse::Group { kind, .. } => assert_eq!(kind, STMT_LIST),
        Reparse::Full => panic!("expected a group reparse"),
    }

    // Every top-level item other than the impl is shared with the old tree.
    let after = document.parse().green();
    let old_items: Vec<_> = before.children().iter().collect();
    let new_items: Vec<_> = after.children().iter().collect();
    assert_eq!(old_items.len(), new_items.len());
    let mut changed = 0;
    for (old, new) in old_items.iter().zip(&new_items) {
        match (old, new) {
            (GreenElement::Node(old), GreenElement::Node(new)) => {
                if !old.ptr_eq(new) {
                    changed += 1;
                    assert_eq!(new.kind(), IMPL);
                }
            }
            (GreenElement::Token(old), GreenElement::Token(new)) => assert_eq!(old, new),
            _ => panic!("tree shape changed"),
        }
    }
    assert_eq!(changed, 1);
}

#[test]
fn an_edit_inside_a_macro_call_reparses_its_token_tree() {
    let mut document = document();
    let at = offset_of(document.text(), "c.get(\"a\")");
    match edit(&mut document, TextRange::empty(at), "1 + ") {
        Reparse::Group { kind, .. } => assert_eq!(kind, TOKEN_TREE),
        Reparse::Full => panic!("expected a group reparse"),
    }
}

#[test]
fn unbalancing_an_edit_falls_back_to_a_full_parse() {
    let mut document = document();
    let at = offset_of(document.text(), "c.add(w);");
    assert_eq!(
        edit(&mut document, TextRange::empty(at), "{"),
        Reparse::Full
    );
    // The file is still parsed (with errors) and further edits work.
    assert!(!document.parse().errors().is_empty());
    let at = offset_of(document.text(), "{c.add");
    edit(&mut document, TextRange::new(at, at + 1), "");
    assert!(document.parse().errors().is_empty());
}

#[test]
fn a_string_swallowing_a_closer_moves_to_an_enclosing_group() {
    let mut document = document();
    // Opening a string inside `get`'s body makes it run past the match arm
    // list's closer, so that group cannot absorb the edit.
    let at = offset_of(document.text(), "None => 0");
    let reparse = edit(&mut document, TextRange::empty(at), "\"");
    assert!(
        !matches!(
            reparse,
            Reparse::Group {
                kind: MATCH_ARM_LIST,
                ..
            }
        ),
        "{reparse:?}"
    );
}

#[test]
fn edits_at_group_boundaries() {
    let mut document = document();
    // Right after an opener and right before a closer are inside the group.
    let open = offset_of(document.text(), "fn main() {") + 11;
    edit(&mut document, TextRange::empty(open), " let z = 0;");
    let close = offset_of(document.text(), "    println!") - 1;
    edit(&mut document, TextRange::empty(close), "z;");
    // Deleting a closer is not.
    let main = offset_of(document.text(), "fn main()");
    let end = document.text()[main as usize..].rfind('}').unwrap() as u32 + main;
    assert_eq!(
        edit(&mut document, TextRange::new(end, end + 1), ""),
        Reparse::Full
    );
}

/// Random edits, each checked against a fresh parse.
#[test]
fn random_edits_match_fresh_parses() {
    const SNIPPETS: &[&str] = &[
        " ",
        "x",
        "1",
        ";",
        "{",
        "}",
        "(",
        ")",
        "[",
        "]",
        "\"",
        "'",
        "/*",
        "*/",
        "//",
        "\n",
        "let y = 2;",
        "foo()",
        "a.0.1",
        "<",
        ">",
        "::",
        ",",
        "=>",
        "fn",
        "if x {}",
        "r#\"",
        "'a",
    ];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = |n: usize| {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as usize % n.max(1)
    };
    let mut groups = 0;
    for _ in 0..20 {
        let mut document = document();
        for _ in 0..40 {
            let len = document.text().len();
            let start = next(len + 1);
            let end = if next(3) == 0 {
                (start + 1 + next(6)).min(len)
            } else {
                start
            };
            let insert = if end > start && next(2) == 0 {
                ""
            } else {
                SNIPPETS[next(SNIPPETS.len())]
            };
            if let Reparse::Group { .. } = edit(
                &mut document,
                TextRange::new(start as u32, end as u32),
                insert,
            ) {
                groups += 1;
            }
        }
    }
    assert!(
        groups > 0,
        "the random edits never exercised a group reparse"
    );
}
