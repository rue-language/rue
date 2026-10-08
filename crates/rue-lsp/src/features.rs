//! LSP result shapes projected from [`ModuleIndex`] records.

use serde_json::{Value, json};

use crate::syntax_index::{Definition, DefinitionKind, ModuleIndex, Node};
use crate::text::{Range, path_to_uri};

// LSP `SymbolKind` values.
const SYMBOL_NAMESPACE: u32 = 3;
const SYMBOL_METHOD: u32 = 6;
const SYMBOL_FIELD: u32 = 8;
const SYMBOL_ENUM: u32 = 10;
const SYMBOL_INTERFACE: u32 = 11;
const SYMBOL_FUNCTION: u32 = 12;
const SYMBOL_VARIABLE: u32 = 13;
const SYMBOL_CONSTANT: u32 = 14;
const SYMBOL_ENUM_MEMBER: u32 = 22;
const SYMBOL_STRUCT: u32 = 23;
const SYMBOL_TYPE_PARAMETER: u32 = 26;

// LSP `CompletionItemKind` values.
const COMPLETION_METHOD: u32 = 2;
const COMPLETION_FUNCTION: u32 = 3;
const COMPLETION_FIELD: u32 = 5;
const COMPLETION_VARIABLE: u32 = 6;
const COMPLETION_INTERFACE: u32 = 8;
const COMPLETION_KEYWORD: u32 = 14;
const COMPLETION_ENUM: u32 = 13;
const COMPLETION_CONSTANT: u32 = 21;
const COMPLETION_STRUCT: u32 = 22;
const COMPLETION_ENUM_MEMBER: u32 = 20;
const COMPLETION_TYPE_PARAMETER: u32 = 25;

pub fn range_of(index: &ModuleIndex, start: usize, end: usize) -> Range {
    index.lines.range(&index.text, start, end)
}

pub fn location(index: &ModuleIndex, definition: &Definition) -> Value {
    json!({
        "uri": path_to_uri(&index.path),
        "range": range_of(index, definition.name_start, definition.name_end).to_json(),
    })
}

/// The hierarchical outline of one module.
pub fn document_symbols(index: &ModuleIndex) -> Vec<Value> {
    index
        .items
        .iter()
        .flat_map(|item| item_symbols(index, item))
        .collect()
}

fn item_symbols(index: &ModuleIndex, item: &Node) -> Vec<Value> {
    let (kind, detail) = match item.kind.as_str() {
        "function" => (SYMBOL_FUNCTION, None),
        "struct" => (SYMBOL_STRUCT, None),
        "enum" => (SYMBOL_ENUM, None),
        "interface" => (SYMBOL_INTERFACE, None),
        "const" => (SYMBOL_CONSTANT, None),
        "test" => (SYMBOL_FUNCTION, Some("test")),
        "drop_function" => (SYMBOL_FUNCTION, Some("drop")),
        "extern" => {
            let children: Vec<Value> = item
                .children
                .iter()
                .filter(|child| child.kind == "extern_function")
                .filter_map(|child| symbol(index, child, SYMBOL_FUNCTION, None, Vec::new()))
                .collect();
            return vec![json!({
                "name": "extern",
                "kind": SYMBOL_NAMESPACE,
                "range": range_of(index, item.start, item.end).to_json(),
                "selectionRange": range_of(index, item.start, item.start).to_json(),
                "children": children,
            })];
        }
        _ => return Vec::new(),
    };
    let children = item
        .children
        .iter()
        .filter_map(|child| {
            let kind = match child.kind.as_str() {
                "field" => SYMBOL_FIELD,
                "method" | "interface_requirement" => SYMBOL_METHOD,
                "enum_variant" => SYMBOL_ENUM_MEMBER,
                "associated_type_requirement" => SYMBOL_TYPE_PARAMETER,
                _ => return None,
            };
            symbol(index, child, kind, None, Vec::new())
        })
        .collect();
    symbol(index, item, kind, detail, children)
        .into_iter()
        .collect()
}

fn symbol(
    index: &ModuleIndex,
    node: &Node,
    kind: u32,
    detail: Option<&str>,
    children: Vec<Value>,
) -> Option<Value> {
    let name = node.name.as_deref().filter(|name| !name.is_empty())?;
    let selection = index
        .tokens
        .iter()
        .skip_while(|token| token.start < node.start)
        .take_while(|token| token.start < node.end)
        .find(|token| token.value.as_deref() == Some(name))
        .map_or((node.start, node.start), |token| (token.start, token.end));
    let mut value = json!({
        "name": name,
        "kind": kind,
        "range": range_of(index, node.start, node.end).to_json(),
        "selectionRange": range_of(index, selection.0, selection.1).to_json(),
    });
    if let Some(detail) = detail {
        value["detail"] = json!(detail);
    }
    if !children.is_empty() {
        value["children"] = Value::Array(children);
    }
    Some(value)
}

/// Markdown hover contents for a declaration.
pub fn hover(index: &ModuleIndex, definition: &Definition) -> String {
    let mut markdown = String::new();
    if let Some(container) = &definition.container {
        markdown.push_str(&format!("`{container}`\n\n"));
    }
    markdown.push_str("```rue\n");
    markdown.push_str(&index.signature(definition));
    markdown.push_str("\n```");
    if let Some(doc) = index.doc_comment(definition) {
        markdown.push_str("\n\n---\n\n");
        markdown.push_str(&doc);
    }
    markdown
}

/// The semantic token legend the server registers. Indices into this list
/// are the token types [`semantic_tokens`] reports.
pub const SEMANTIC_TOKEN_TYPES: &[&str] = &[
    "keyword",
    "type",
    "function",
    "variable",
    "number",
    "string",
    "operator",
    "enumMember",
    "property",
    "method",
    "parameter",
];

const TOKEN_KEYWORD: u32 = 0;
const TOKEN_TYPE: u32 = 1;
const TOKEN_FUNCTION: u32 = 2;
const TOKEN_VARIABLE: u32 = 3;
const TOKEN_NUMBER: u32 = 4;
const TOKEN_STRING: u32 = 5;
const TOKEN_OPERATOR: u32 = 6;
const TOKEN_ENUM_MEMBER: u32 = 7;
const TOKEN_PROPERTY: u32 = 8;
const TOKEN_METHOD: u32 = 9;

/// Full-document semantic tokens in the protocol's relative encoding.
///
/// Classification is syntactic: keywords, literals, and operators come from
/// the token kind, and an identifier is classified by its neighbors and by
/// the module's own type declarations. Comments are not tokens, so the
/// client's grammar keeps highlighting them.
pub fn semantic_tokens(index: &ModuleIndex) -> Vec<u32> {
    let type_names: std::collections::HashSet<&str> = index
        .declarations()
        .iter()
        .filter(|definition| {
            matches!(
                definition.kind,
                DefinitionKind::Struct | DefinitionKind::Enum | DefinitionKind::Interface
            )
        })
        .map(|definition| definition.name.as_str())
        .collect();
    let mut data = Vec::new();
    let mut previous_line = 0;
    let mut previous_start = 0;
    for (position, token) in index.tokens.iter().enumerate() {
        let neighbor = |delta: isize| {
            position
                .checked_add_signed(delta)
                .and_then(|neighbor| index.tokens.get(neighbor))
                .map(|token| token.kind.as_str())
        };
        let kind = token.kind.as_str();
        let token_type = match kind {
            "IDENT" => {
                let name = token.value.as_deref().unwrap_or_default();
                match (neighbor(-1), neighbor(1)) {
                    (Some("DOT"), Some("LPAREN")) => TOKEN_METHOD,
                    (Some("DOT"), _) => TOKEN_PROPERTY,
                    (Some("COLONCOLON"), _) => TOKEN_ENUM_MEMBER,
                    (Some("FN"), _) | (_, Some("LPAREN")) => TOKEN_FUNCTION,
                    _ if type_names.contains(name) => TOKEN_TYPE,
                    (_, Some("COLONCOLON")) => TOKEN_TYPE,
                    _ => TOKEN_VARIABLE,
                }
            }
            "INT" | "FLOAT" => TOKEN_NUMBER,
            "STRING" => TOKEN_STRING,
            "TRUE" | "FALSE" | "SELF" | "SELFTYPE" => TOKEN_KEYWORD,
            _ if kind.starts_with("TYPE(") => TOKEN_TYPE,
            "LPAREN" | "RPAREN" | "LBRACE" | "RBRACE" | "LBRACKET" | "RBRACKET" | "COMMA"
            | "SEMI" | "COLON" | "DOT" | "UNDERSCORE" | "AT" => continue,
            _ if kind.chars().all(|ch| ch.is_ascii_uppercase()) && is_keyword(kind) => {
                TOKEN_KEYWORD
            }
            _ => TOKEN_OPERATOR,
        };
        let start = index.lines.position(&index.text, token.start);
        let end = index.lines.position(&index.text, token.end);
        // Semantic tokens cannot span lines; a multi-line string literal is
        // left to the client's grammar.
        if start.line != end.line {
            continue;
        }
        let delta_line = start.line - previous_line;
        let delta_start = if delta_line == 0 {
            start.character - previous_start
        } else {
            start.character
        };
        data.extend([
            delta_line,
            delta_start,
            end.character - start.character,
            token_type,
            0,
        ]);
        previous_line = start.line;
        previous_start = start.character;
    }
    data
}

fn is_keyword(kind: &str) -> bool {
    KEYWORDS
        .iter()
        .any(|keyword| keyword.eq_ignore_ascii_case(kind))
}

/// Rue's reserved words, matching the lexer's keyword token kinds.
const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "inout",
    "borrow",
    "if",
    "else",
    "match",
    "while",
    "loop",
    "yield",
    "for",
    "in",
    "break",
    "continue",
    "return",
    "true",
    "false",
    "struct",
    "enum",
    "interface",
    "impl",
    "drop",
    "linear",
    "self",
    "Self",
    "comptime",
    "pub",
    "const",
    "checked",
    "unchecked",
    "ptr",
    "extern",
    "test",
];

const BUILTIN_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "bool", "f32", "f64", "type",
];

/// Completion candidates at `offset`: binders in scope, the module's own
/// declarations, built-in types, and keywords. A member position (after a
/// `.`) offers the fields and methods of the module's types instead, since
/// the receiver's type is not known syntactically.
pub fn completions(index: &ModuleIndex, offset: usize, after_dot: bool) -> Vec<Value> {
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |label: &str, kind: u32, detail: Option<String>| {
        if seen.insert((label.to_owned(), kind)) {
            let mut item = json!({ "label": label, "kind": kind });
            if let Some(detail) = detail {
                item["detail"] = json!(detail);
            }
            items.push(item);
        }
    };
    let declarations = index.declarations();
    if after_dot {
        for definition in declarations {
            let kind = match definition.kind {
                DefinitionKind::Field => COMPLETION_FIELD,
                DefinitionKind::Method | DefinitionKind::InterfaceMethod => COMPLETION_METHOD,
                _ => continue,
            };
            push(&definition.name, kind, definition.container.clone());
        }
        return items;
    }
    for local in index.locals_at(offset).iter().rev() {
        push(&local.name, COMPLETION_VARIABLE, None);
    }
    for definition in declarations {
        let kind = match definition.kind {
            DefinitionKind::Function => COMPLETION_FUNCTION,
            DefinitionKind::Struct => COMPLETION_STRUCT,
            DefinitionKind::Enum => COMPLETION_ENUM,
            DefinitionKind::Interface => COMPLETION_INTERFACE,
            DefinitionKind::Const => COMPLETION_CONSTANT,
            DefinitionKind::Variant => COMPLETION_ENUM_MEMBER,
            _ => continue,
        };
        let detail = match definition.kind {
            DefinitionKind::Function => Some(index.signature(definition)),
            _ => definition.container.clone(),
        };
        push(&definition.name, kind, detail);
    }
    for builtin in BUILTIN_TYPES {
        push(
            builtin,
            COMPLETION_TYPE_PARAMETER,
            Some("built-in type".into()),
        );
    }
    for keyword in KEYWORDS {
        push(keyword, COMPLETION_KEYWORD, None);
    }
    items
}

/// The LSP `SymbolKind` of a declaration, for workspace symbol search.
pub fn symbol_kind(kind: DefinitionKind) -> u32 {
    match kind {
        DefinitionKind::Function => SYMBOL_FUNCTION,
        DefinitionKind::Struct => SYMBOL_STRUCT,
        DefinitionKind::Enum => SYMBOL_ENUM,
        DefinitionKind::Interface => SYMBOL_INTERFACE,
        DefinitionKind::Const => SYMBOL_CONSTANT,
        DefinitionKind::Field => SYMBOL_FIELD,
        DefinitionKind::Method | DefinitionKind::InterfaceMethod => SYMBOL_METHOD,
        DefinitionKind::Variant => SYMBOL_ENUM_MEMBER,
        DefinitionKind::Parameter | DefinitionKind::Local => SYMBOL_VARIABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax_index::tests::index;

    #[test]
    fn the_outline_nests_members_under_their_type() {
        let index =
            index("struct P { x: i32, fn get(self) -> i32 { self.x } }\nfn main() -> i32 { 0 }\n");
        let symbols = document_symbols(&index);
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["name"], "P");
        assert_eq!(symbols[0]["kind"], SYMBOL_STRUCT);
        let children = symbols[0]["children"].as_array().unwrap();
        assert_eq!(children[0]["name"], "x");
        assert_eq!(children[1]["name"], "get");
        assert_eq!(symbols[1]["name"], "main");
        assert_eq!(
            symbols[1]["selectionRange"]["start"],
            json!({"line": 1, "character": 3})
        );
    }

    #[test]
    fn semantic_tokens_classify_by_token_kind_and_neighbors() {
        let index = index("fn main() -> i32 {\n    let x = 1;\n    x\n}\n");
        let tokens = semantic_tokens(&index);
        assert_eq!(tokens.len() % 5, 0);
        let types: Vec<u32> = tokens.chunks(5).map(|chunk| chunk[3]).collect();
        // fn main -> i32 let x = 1 x
        assert_eq!(
            types,
            vec![
                TOKEN_KEYWORD,
                TOKEN_FUNCTION,
                TOKEN_OPERATOR,
                TOKEN_TYPE,
                TOKEN_KEYWORD,
                TOKEN_VARIABLE,
                TOKEN_OPERATOR,
                TOKEN_NUMBER,
                TOKEN_VARIABLE
            ]
        );
        // `let` on line 1, column 4.
        assert_eq!(&tokens[20..22], &[1, 4]);
    }

    #[test]
    fn completion_offers_scope_then_declarations() {
        let source = "fn helper() -> i32 { 1 }\nfn main() -> i32 {\n    let value = 2;\n    \n}\n";
        let index = index(source);
        let offset = source.find("    \n}").unwrap() + 4;
        let labels: Vec<String> = completions(&index, offset, false)
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(labels[0], "value");
        assert!(labels.contains(&"helper".to_owned()));
        assert!(labels.contains(&"i32".to_owned()));
        assert!(labels.contains(&"while".to_owned()));
    }
}
