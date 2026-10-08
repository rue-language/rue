//! Navigation over the compiler's canonical syntax view.
//!
//! Every syntactic answer the server gives — outline, definitions, hover
//! headers, highlighting, completion candidates — is a projection of one
//! `rue_compiler::SyntaxModuleView`. The server never runs its own lexer or
//! parser; [`ModuleIndex`] only copies the view into owned records so a
//! request can walk it without re-materializing node views.

use std::path::PathBuf;

use rue_compiler::{SyntaxModuleView, SyntaxNodeView};

use crate::text::LineIndex;

/// One token of the canonical token view.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: String,
    pub value: Option<String>,
    pub start: usize,
    pub end: usize,
}

/// One node of the canonical syntax view.
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: String,
    pub name: Option<String>,
    pub start: usize,
    pub end: usize,
    pub children: Vec<Node>,
}

impl Node {
    fn from_view(view: SyntaxNodeView) -> Self {
        let location = view.location();
        Self {
            kind: view.kind().to_owned(),
            name: view.name().map(str::to_owned),
            start: location.start() as usize,
            end: location.end() as usize,
            children: view.children().map(Node::from_view).collect(),
        }
    }

    fn contains(&self, offset: usize) -> bool {
        self.start <= offset && offset <= self.end
    }
}

/// What a name resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    Function,
    Struct,
    Enum,
    Interface,
    Const,
    Field,
    Method,
    Variant,
    InterfaceMethod,
    Parameter,
    Local,
}

impl DefinitionKind {
    fn is_member(self) -> bool {
        matches!(
            self,
            Self::Field | Self::Method | Self::Variant | Self::InterfaceMethod
        )
    }
}

/// A declaration found in a module.
#[derive(Clone, Debug)]
pub struct Definition {
    pub kind: DefinitionKind,
    pub name: String,
    /// The whole declaration.
    pub start: usize,
    pub end: usize,
    /// The declaration's name.
    pub name_start: usize,
    pub name_end: usize,
    /// The enclosing type's name, for a member.
    pub container: Option<String>,
}

/// Where the identifier under the cursor sits syntactically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    /// A plain name: a local, a parameter, or a module-level item.
    Name,
    /// The member name of `receiver.name` or `receiver.name(...)`.
    Member,
    /// The variant name of `Type::Variant`.
    Variant,
}

/// Owned navigation records for one module of a syntax view.
pub struct ModuleIndex {
    pub path: PathBuf,
    pub text: String,
    pub lines: LineIndex,
    pub tokens: Vec<Token>,
    pub items: Vec<Node>,
    declarations: Vec<Definition>,
}

impl ModuleIndex {
    /// Index one module. `path` is the module's absolute path; the view
    /// spells it as its owner was given it.
    pub fn new(module: &SyntaxModuleView, path: PathBuf) -> Self {
        let text = module.source().to_owned();
        let mut index = Self {
            path,
            lines: LineIndex::new(&text),
            tokens: module
                .tokens()
                .filter(|token| token.kind() != "EOF")
                .map(|token| Token {
                    kind: token.kind().to_owned(),
                    value: token.value().map(|value| value.into_owned()),
                    start: token.start() as usize,
                    end: token.end() as usize,
                })
                .collect(),
            items: module.nodes().map(Node::from_view).collect(),
            text,
            declarations: Vec::new(),
        };
        index.declarations = index.collect_declarations();
        index
    }

    /// The index of the token touching `offset`. A cursor just past an
    /// identifier (the usual position after typing it) selects the
    /// identifier, even when punctuation starts right there.
    pub fn token_at(&self, offset: usize) -> Option<usize> {
        let after = self.tokens.partition_point(|token| token.start <= offset);
        let candidate = after.checked_sub(1)?;
        let token = &self.tokens[candidate];
        let ending_identifier = || {
            let previous = candidate.checked_sub(1)?;
            let token = &self.tokens[previous];
            (token.end == offset && token.kind == "IDENT").then_some(previous)
        };
        if offset < token.end {
            if token.kind != "IDENT"
                && let Some(previous) = ending_identifier()
            {
                return Some(previous);
            }
            return Some(candidate);
        }
        (offset == token.end && token.kind == "IDENT").then_some(candidate)
    }

    /// The identifier under the cursor and its syntactic role.
    pub fn identifier_at(&self, offset: usize) -> Option<(usize, &str, Reference)> {
        let index = self.token_at(offset)?;
        let token = &self.tokens[index];
        if token.kind != "IDENT" {
            return None;
        }
        let name = token.value.as_deref()?;
        let previous = index
            .checked_sub(1)
            .map(|previous| self.tokens[previous].kind.as_str());
        let reference = match previous {
            Some("DOT") => Reference::Member,
            Some("COLONCOLON") => Reference::Variant,
            _ => Reference::Name,
        };
        Some((index, name, reference))
    }

    /// The import specification behind a module alias: `x` for
    /// `const x = @import("spec");`.
    pub fn import_alias(&self, alias: &str) -> Option<&str> {
        self.tokens.windows(8).find_map(|window| {
            let kinds: Vec<&str> = window.iter().map(|token| token.kind.as_str()).collect();
            let shape = [
                "CONST", "IDENT", "EQ", "AT", "IDENT", "LPAREN", "STRING", "RPAREN",
            ];
            (kinds == shape
                && window[1].value.as_deref() == Some(alias)
                && window[4].value.as_deref() == Some("import"))
            .then(|| window[6].value.as_deref())
            .flatten()
        })
    }

    /// For a member reference `receiver.name`, the receiver's identifier.
    pub fn receiver_of(&self, token: usize) -> Option<&str> {
        let dot = token.checked_sub(1)?;
        let receiver = token.checked_sub(2)?;
        (self.tokens[dot].kind == "DOT" && self.tokens[receiver].kind == "IDENT")
            .then(|| self.tokens[receiver].value.as_deref())
            .flatten()
    }

    /// The first token inside `[start, end)` that spells `name`: the name of
    /// a declaration whose node spans the whole declaration.
    fn name_token(&self, start: usize, end: usize, name: &str) -> Option<&Token> {
        let first = self.tokens.partition_point(|token| token.start < start);
        self.tokens[first..]
            .iter()
            .take_while(|token| token.start < end)
            .find(|token| {
                matches!(token.kind.as_str(), "IDENT" | "STRING")
                    && token.value.as_deref() == Some(name)
            })
    }

    fn definition(
        &self,
        node: &Node,
        kind: DefinitionKind,
        container: Option<&str>,
    ) -> Option<Definition> {
        let name = node.name.as_deref()?;
        let (name_start, name_end) = self
            .name_token(node.start, node.end, name)
            .map_or((node.start, node.end), |token| (token.start, token.end));
        Some(Definition {
            kind,
            name: name.to_owned(),
            start: node.start,
            end: node.end,
            name_start,
            name_end,
            container: container.map(str::to_owned),
        })
    }

    /// Every module-level declaration and every member of a module-level
    /// type, in source order.
    pub fn declarations(&self) -> &[Definition] {
        &self.declarations
    }

    fn collect_declarations(&self) -> Vec<Definition> {
        let mut found = Vec::new();
        for item in &self.items {
            let kind = match item.kind.as_str() {
                "function" => DefinitionKind::Function,
                "struct" => DefinitionKind::Struct,
                "enum" => DefinitionKind::Enum,
                "interface" => DefinitionKind::Interface,
                "const" => DefinitionKind::Const,
                "extern" => {
                    found.extend(
                        item.children
                            .iter()
                            .filter(|child| child.kind == "extern_function")
                            .filter_map(|child| {
                                self.definition(child, DefinitionKind::Function, None)
                            }),
                    );
                    continue;
                }
                _ => continue,
            };
            let Some(definition) = self.definition(item, kind, None) else {
                continue;
            };
            let container = definition.name.clone();
            found.push(definition);
            for child in &item.children {
                let member = match child.kind.as_str() {
                    "field" => DefinitionKind::Field,
                    "method" => DefinitionKind::Method,
                    "enum_variant" => DefinitionKind::Variant,
                    "interface_requirement" => DefinitionKind::InterfaceMethod,
                    _ => continue,
                };
                found.extend(self.definition(child, member, Some(&container)));
            }
        }
        found
    }

    /// Module-level declarations or members named `name` that a reference
    /// of this role can denote.
    pub fn declarations_named(&self, name: &str, reference: Reference) -> Vec<Definition> {
        self.declarations
            .iter()
            .filter(|definition| definition.name == name)
            .filter(|definition| match reference {
                Reference::Name => !definition.kind.is_member(),
                Reference::Member => matches!(
                    definition.kind,
                    DefinitionKind::Field
                        | DefinitionKind::Method
                        | DefinitionKind::InterfaceMethod
                ),
                Reference::Variant => definition.kind == DefinitionKind::Variant,
            })
            .cloned()
            .collect()
    }

    /// The binder `name` refers to at `offset`, following lexical scope:
    /// parameters, then `let`s earlier in an enclosing block, `for` binders
    /// inside their body, and pattern bindings inside their match arm. The
    /// innermost, latest binder wins, so shadowing resolves as the compiler
    /// resolves it.
    pub fn local_at(&self, offset: usize, name: &str) -> Option<Definition> {
        let mut scope = Vec::new();
        for item in &self.items {
            if item.contains(offset) {
                self.collect_scope(item, offset, &mut scope);
            }
        }
        scope.into_iter().rev().find(|binder| binder.name == name)
    }

    /// Every binder visible at `offset`, innermost last.
    pub fn locals_at(&self, offset: usize) -> Vec<Definition> {
        let mut scope = Vec::new();
        for item in &self.items {
            if item.contains(offset) {
                self.collect_scope(item, offset, &mut scope);
            }
        }
        scope
    }

    fn collect_scope(&self, node: &Node, offset: usize, scope: &mut Vec<Definition>) {
        match node.kind.as_str() {
            "function" | "method" | "drop_function" | "test" | "struct" => {
                for child in &node.children {
                    if child.kind == "parameter" {
                        scope.extend(self.definition(child, DefinitionKind::Parameter, None));
                    } else if child.contains(offset) {
                        self.collect_scope(child, offset, scope);
                    }
                }
            }
            "block" => {
                for child in &node.children {
                    if child.contains(offset) {
                        self.collect_scope(child, offset, scope);
                        break;
                    }
                    if child.end <= offset {
                        self.collect_binders(child, scope);
                    }
                }
            }
            "for" | "match_arm" => {
                // The first child is the binder or pattern; it scopes over
                // the rest.
                let mut children = node.children.iter();
                if let Some(binder) = children.next() {
                    if binder.contains(offset) {
                        return;
                    }
                    let rest: Vec<_> = children.collect();
                    if rest.last().is_some_and(|body| body.contains(offset)) {
                        self.collect_pattern(binder, scope);
                    }
                    for child in rest {
                        if child.contains(offset) {
                            self.collect_scope(child, offset, scope);
                            break;
                        }
                    }
                }
            }
            _ => {
                for child in &node.children {
                    if child.contains(offset) {
                        self.collect_scope(child, offset, scope);
                        break;
                    }
                }
            }
        }
    }

    /// The binders a completed block statement introduces.
    fn collect_binders(&self, statement: &Node, scope: &mut Vec<Definition>) {
        match statement.kind.as_str() {
            "let" => scope.extend(self.definition(statement, DefinitionKind::Local, None)),
            "let_struct_pattern" => {
                for child in &statement.children {
                    if child.kind == "struct_pattern" {
                        self.collect_pattern(child, scope);
                    }
                }
            }
            _ => {}
        }
    }

    fn collect_pattern(&self, pattern: &Node, scope: &mut Vec<Definition>) {
        if matches!(pattern.kind.as_str(), "binding" | "binding_pattern") {
            scope.extend(self.definition(pattern, DefinitionKind::Local, None));
        }
        for child in &pattern.children {
            self.collect_pattern(child, scope);
        }
    }

    /// The source a hover shows for a declaration: a function's signature
    /// without its body, or a short declaration in full.
    pub fn signature(&self, definition: &Definition) -> String {
        const MAX_LINES: usize = 24;
        let mut end = definition.end;
        if matches!(
            definition.kind,
            DefinitionKind::Function | DefinitionKind::Method
        ) {
            // The body is the declaration's last child, a block.
            let body = self
                .node_spanning(definition.start, definition.end)
                .and_then(|node| node.children.last())
                .filter(|body| body.kind == "block");
            if let Some(body) = body {
                end = body.start;
            }
        }
        let text = self.text[definition.start..end.min(self.text.len())].trim_end();
        let mut lines = text.lines();
        let mut shown: Vec<&str> = lines.by_ref().take(MAX_LINES).collect();
        if lines.next().is_some() {
            shown.push("    // ...");
        }
        shown.join("\n")
    }

    fn node_spanning(&self, start: usize, end: usize) -> Option<&Node> {
        fn find(nodes: &[Node], start: usize, end: usize) -> Option<&Node> {
            for node in nodes {
                if node.start == start && node.end == end {
                    return Some(node);
                }
                if node.start <= start
                    && end <= node.end
                    && let Some(found) = find(&node.children, start, end)
                {
                    return Some(found);
                }
            }
            None
        }
        find(&self.items, start, end)
    }

    /// The `///` comment lines directly above a declaration.
    pub fn doc_comment(&self, definition: &Definition) -> Option<String> {
        let before = &self.text[..definition.start];
        let mut lines: Vec<&str> = Vec::new();
        // The declaration's own line up to its start is indentation.
        let mut rest = before.rsplit_once('\n').map(|(head, _)| head);
        while let Some(text) = rest {
            let (head, line) = match text.rsplit_once('\n') {
                Some((head, line)) => (Some(head), line),
                None => (None, text),
            };
            let Some(comment) = line.trim_start().strip_prefix("///") else {
                break;
            };
            lines.push(comment.strip_prefix(' ').unwrap_or(comment));
            rest = head;
        }
        if lines.is_empty() {
            return None;
        }
        lines.reverse();
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use ahash::AHashMap;
    use rue_compiler::{CompilerSession, FileId, SourceMetadata, SourceSnapshot};

    use super::*;

    pub(crate) fn index(source: &str) -> ModuleIndex {
        let root = FileId::new(1);
        let paths = AHashMap::from([(root, "main.rue".to_owned())]);
        let metadata = SourceMetadata::new(root, paths.clone(), paths).unwrap();
        let snapshot =
            SourceSnapshot::new(metadata, vec![(root, Arc::new(source.to_owned()))]).unwrap();
        let syntax = CompilerSession::new()
            .update(&snapshot)
            .into_result()
            .unwrap();
        let module = syntax.modules().next().unwrap();
        ModuleIndex::new(&module, PathBuf::from("/main.rue"))
    }

    fn offset_of(text: &str, needle: &str, nth: usize) -> usize {
        text.match_indices(needle).nth(nth).unwrap().0
    }

    const PROGRAM: &str = "\
/// A point.
struct Point {
    x: i32,
    y: i32,

    fn sum(self) -> i32 { self.x + self.y }
}

enum Shape { Dot, Line(i32) }

fn helper(x: i32) -> i32 {
    let y = x + 1;
    let x = y * 2;
    x
}

fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    helper(p.sum())
}
";

    #[test]
    fn declarations_cover_items_and_members() {
        let index = index(PROGRAM);
        let names: Vec<_> = index
            .declarations()
            .iter()
            .map(|definition| {
                (
                    definition.kind,
                    definition.name.clone(),
                    definition.container.clone(),
                )
            })
            .collect();
        assert!(names.contains(&(DefinitionKind::Struct, "Point".into(), None)));
        assert!(names.contains(&(DefinitionKind::Field, "x".into(), Some("Point".into()))));
        assert!(names.contains(&(DefinitionKind::Method, "sum".into(), Some("Point".into()))));
        assert!(names.contains(&(DefinitionKind::Variant, "Line".into(), Some("Shape".into()))));
        assert!(names.contains(&(DefinitionKind::Function, "main".into(), None)));
    }

    #[test]
    fn a_declaration_name_is_its_name_token() {
        let index = index(PROGRAM);
        let helper = &index.declarations_named("helper", Reference::Name)[0];
        assert_eq!(&PROGRAM[helper.name_start..helper.name_end], "helper");
        assert_eq!(
            index.signature(helper),
            "fn helper(x: i32) -> i32",
            "a function's hover omits its body"
        );
        let point = &index.declarations_named("Point", Reference::Name)[0];
        assert_eq!(index.doc_comment(point).as_deref(), Some("A point."));
    }

    #[test]
    fn locals_follow_lexical_scope_and_shadowing() {
        let index = index(PROGRAM);
        // `x` in `let y = x + 1` is the parameter.
        let use_of_param = offset_of(PROGRAM, "x + 1", 0);
        let binder = index.local_at(use_of_param, "x").unwrap();
        assert_eq!(binder.kind, DefinitionKind::Parameter);
        // The trailing `x` is the shadowing `let x`.
        let trailing = offset_of(PROGRAM, "    x\n}", 0) + 4;
        let binder = index.local_at(trailing, "x").unwrap();
        assert_eq!(binder.kind, DefinitionKind::Local);
        assert_eq!(binder.name_start, offset_of(PROGRAM, "x = y * 2", 0));
        // `y` is not visible in `main`.
        let in_main = offset_of(PROGRAM, "helper(p", 0);
        assert!(index.local_at(in_main, "y").is_none());
        assert!(index.local_at(in_main, "p").is_some());
    }

    #[test]
    fn identifiers_know_their_syntactic_role() {
        let index = index(PROGRAM);
        let sum_call = offset_of(PROGRAM, "sum()", 0);
        let (_, name, reference) = index.identifier_at(sum_call).unwrap();
        assert_eq!((name, reference), ("sum", Reference::Member));
        // The cursor just past an identifier still selects it.
        let (_, name, _) = index.identifier_at(sum_call + 3).unwrap();
        assert_eq!(name, "sum");
        let members = index.declarations_named("sum", Reference::Member);
        assert_eq!(members.len(), 1);
    }
}
