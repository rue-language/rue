//! Request dispatch and server state.
//!
//! [`Server::handle`] maps one incoming JSON-RPC message to the messages it
//! causes — a response, published diagnostics, or both — so the protocol is
//! testable without a transport.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rue_compiler::{PreviewFeature, PreviewFeatures, RootSelection};
use rue_driver::HostPathContext;
use serde_json::{Value, json};

use crate::check::{CheckSettings, Checker};
use crate::document::Document;
use crate::features;
use crate::syntax_index::{Definition, ModuleIndex, Reference};
use crate::text::{Position, path_to_uri, uri_to_path};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const SERVER_NOT_INITIALIZED: i64 = -32002;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    Uninitialized,
    Running,
    ShutDown,
}

pub struct Server {
    lifecycle: Lifecycle,
    /// The client's workspace folder, which bounds the search for an
    /// enclosing `main.rue`.
    workspace: Option<PathBuf>,
    /// An explicit program root (`initializationOptions.root`). Every file is
    /// then checked as part of that one program.
    configured_root: Option<PathBuf>,
    checker: Option<Checker>,
    documents: HashMap<String, Document>,
    /// The latest whole-program diagnostics per file.
    check_diagnostics: HashMap<PathBuf, Vec<Value>>,
    /// The files of each checked program's latest closure.
    closures: HashMap<(PathBuf, RootSelection), BTreeSet<PathBuf>>,
    /// Files whose last published diagnostics were not empty, so a later
    /// empty result is published to clear them.
    shown: HashSet<String>,
    exit_code: Option<i32>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Self {
            lifecycle: Lifecycle::Uninitialized,
            workspace: None,
            configured_root: None,
            checker: None,
            documents: HashMap::new(),
            check_diagnostics: HashMap::new(),
            closures: HashMap::new(),
            shown: HashSet::new(),
            exit_code: None,
        }
    }

    /// The process exit code once the client has sent `exit`.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// A response to a body that was not JSON.
    pub fn parse_error(message: &str) -> Value {
        error_response(Value::Null, PARSE_ERROR, message)
    }

    pub fn handle(&mut self, message: Value) -> Vec<Value> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to a server-initiated request; the server sends
            // none, so there is nothing to correlate.
            return Vec::new();
        };
        let method = method.to_owned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match message.get("id").cloned() {
            Some(id) => self.request(id, &method, params),
            None => self.notification(&method, params),
        }
    }

    fn request(&mut self, id: Value, method: &str, params: Value) -> Vec<Value> {
        let result = match (self.lifecycle, method) {
            (Lifecycle::Uninitialized, "initialize") => Ok(self.initialize(&params)),
            (Lifecycle::Uninitialized, _) => Err((
                SERVER_NOT_INITIALIZED,
                "the server has not been initialized".to_owned(),
            )),
            (_, "initialize") => Err((INVALID_REQUEST, "initialize was already sent".to_owned())),
            (Lifecycle::ShutDown, _) => {
                Err((INVALID_REQUEST, "the server is shutting down".to_owned()))
            }
            (Lifecycle::Running, "shutdown") => {
                self.lifecycle = Lifecycle::ShutDown;
                Ok(Value::Null)
            }
            (Lifecycle::Running, method) => self.query(method, &params),
        };
        vec![match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error_response(id, code, &message),
        }]
    }

    fn query(&self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        let result = match method {
            "textDocument/documentSymbol" => self.document_symbols(params),
            "textDocument/hover" => self.hover(params),
            "textDocument/definition" => self.definition(params),
            "textDocument/references" => self.references(params, true),
            "textDocument/documentHighlight" => self.references(params, false),
            "textDocument/completion" => self.completion(params),
            "textDocument/semanticTokens/full" => self.semantic_tokens(params),
            "textDocument/codeAction" => Some(code_actions(params)),
            "workspace/symbol" => Some(self.workspace_symbols(params)),
            _ => return Err((METHOD_NOT_FOUND, format!("unsupported method `{method}`"))),
        };
        Ok(result.unwrap_or(Value::Null))
    }

    fn notification(&mut self, method: &str, params: Value) -> Vec<Value> {
        match (self.lifecycle, method) {
            (_, "exit") => {
                self.exit_code = Some(if self.lifecycle == Lifecycle::ShutDown {
                    0
                } else {
                    1
                });
                Vec::new()
            }
            (Lifecycle::Running, "textDocument/didOpen") => {
                let document = &params["textDocument"];
                let (Some(uri), Some(text)) = (document["uri"].as_str(), document["text"].as_str())
                else {
                    return Vec::new();
                };
                let document = Document::open(uri.to_owned(), text.to_owned());
                let uri = uri.to_owned();
                let path = document.path.clone();
                self.documents.insert(uri.clone(), document);
                let mut outgoing = self.publish_for_document(&uri);
                if let Some(path) = path {
                    outgoing.extend(self.check(&path));
                }
                outgoing
            }
            (Lifecycle::Running, "textDocument/didChange") => {
                let Some(uri) = params["textDocument"]["uri"].as_str() else {
                    return Vec::new();
                };
                // Full synchronization: the last change carries the whole text.
                let Some(text) = params["contentChanges"]
                    .as_array()
                    .and_then(|changes| changes.last())
                    .and_then(|change| change["text"].as_str())
                else {
                    return Vec::new();
                };
                let Some(document) = self.documents.get_mut(uri) else {
                    return Vec::new();
                };
                let had_syntax_errors = !document.syntax_diagnostics.is_empty();
                document.update(text.to_owned());
                // Saved-state diagnostics do not change while typing; only a
                // buffer entering, staying in, or leaving a syntax error
                // changes what the file shows.
                if !had_syntax_errors && document.syntax_diagnostics.is_empty() {
                    return Vec::new();
                }
                let uri = uri.to_owned();
                self.publish_for_document(&uri)
            }
            (Lifecycle::Running, "textDocument/didSave") => {
                let Some(path) = params["textDocument"]["uri"].as_str().and_then(uri_to_path)
                else {
                    return Vec::new();
                };
                self.check(&path)
            }
            (Lifecycle::Running, "textDocument/didClose") => {
                let Some(uri) = params["textDocument"]["uri"].as_str() else {
                    return Vec::new();
                };
                let uri = uri.to_owned();
                self.documents.remove(&uri);
                // What remains for the file is its saved state's analysis.
                self.publish_for_document(&uri)
            }
            _ => Vec::new(),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        self.lifecycle = Lifecycle::Running;
        self.workspace = params["workspaceFolders"]
            .as_array()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder["uri"].as_str())
            .or_else(|| params["rootUri"].as_str())
            .and_then(uri_to_path)
            .or_else(|| params["rootPath"].as_str().map(PathBuf::from));
        let options = &params["initializationOptions"];
        let resolve = |spelling: &str| {
            let path = PathBuf::from(spelling);
            match (&self.workspace, path.is_absolute()) {
                (Some(workspace), false) => workspace.join(path),
                _ => path,
            }
        };
        self.configured_root = options["root"].as_str().map(resolve);
        let std_root = options["stdPath"]
            .as_str()
            .map(resolve)
            .or_else(|| std::env::var_os("RUE_STD_PATH").map(PathBuf::from));
        let mut preview_features = PreviewFeatures::new();
        for name in options["preview"].as_array().into_iter().flatten() {
            if let Some(feature) = name
                .as_str()
                .and_then(|name| name.parse::<PreviewFeature>().ok())
            {
                preview_features.insert(feature);
            }
        }
        let path_context = self
            .workspace
            .clone()
            .filter(|workspace| workspace.is_absolute())
            .map_or_else(HostPathContext::capture, |workspace| {
                HostPathContext::from_working_directory(workspace)
            });
        self.checker = path_context.ok().map(|path_context| {
            Checker::new(
                CheckSettings {
                    std_root,
                    preview_features,
                },
                path_context,
            )
        });
        json!({
            "capabilities": {
                "positionEncoding": "utf-16",
                "textDocumentSync": {
                    "openClose": true,
                    "change": 1,
                    "save": { "includeText": false },
                },
                "documentSymbolProvider": true,
                "hoverProvider": true,
                "definitionProvider": true,
                "referencesProvider": true,
                "documentHighlightProvider": true,
                "workspaceSymbolProvider": true,
                "completionProvider": { "triggerCharacters": ["."] },
                "codeActionProvider": { "codeActionKinds": ["quickfix"] },
                "semanticTokensProvider": {
                    "legend": {
                        "tokenTypes": features::SEMANTIC_TOKEN_TYPES,
                        "tokenModifiers": [],
                    },
                    "full": true,
                },
            },
            "serverInfo": {
                "name": "rue-lsp",
                "version": rue_error::VERSION,
            },
        })
    }

    // ---------------------------------------------------------------------
    // Whole-program checking

    /// Which program checks `path`: the configured root; the file itself
    /// when it declares `main`; a retained program that already contains it;
    /// the nearest `main.rue` in an enclosing directory of the workspace; or
    /// else the file alone, rooted at its tests.
    fn root_for(&self, path: &Path) -> (PathBuf, RootSelection) {
        if let Some(root) = &self.configured_root {
            return (root.clone(), RootSelection::Executable);
        }
        let document = self
            .documents
            .values()
            .find(|document| document.path.as_deref() == Some(path));
        if document.is_some_and(Document::declares_main) {
            return (path.to_owned(), RootSelection::Executable);
        }
        if let Some(found) = self
            .checker
            .as_ref()
            .and_then(|checker| checker.project_containing(path))
        {
            return found;
        }
        let mut directory = path.parent();
        while let Some(current) = directory {
            let candidate = current.join("main.rue");
            if candidate != path && candidate.is_file() {
                return (candidate, RootSelection::Executable);
            }
            if self.workspace.as_deref() == Some(current) {
                break;
            }
            directory = current.parent();
        }
        let selection = if document.is_some_and(Document::declares_tests) {
            RootSelection::Tests
        } else {
            RootSelection::Executable
        };
        (path.to_owned(), selection)
    }

    fn check(&mut self, path: &Path) -> Vec<Value> {
        let (root, selection) = self.root_for(path);
        let Some(checker) = self.checker.as_mut() else {
            return Vec::new();
        };
        let outcome = checker.check(&root, selection);
        let files: BTreeSet<PathBuf> = outcome.files.keys().cloned().collect();
        // A file that left this program's closure keeps no diagnostics from it.
        let previous = self
            .closures
            .insert((root, selection), files.clone())
            .unwrap_or_default();
        for file in previous.difference(&files) {
            self.check_diagnostics.remove(file);
        }
        self.check_diagnostics.extend(outcome.files);
        previous
            .union(&files)
            .filter_map(|file| self.publish_path(file))
            .collect()
    }

    // ---------------------------------------------------------------------
    // Diagnostics publication

    /// The diagnostics a file shows: the live buffer's syntax errors while it
    /// does not parse, otherwise the latest whole-program analysis.
    fn diagnostics_for(&self, uri: &str, path: Option<&Path>) -> Vec<Value> {
        if let Some(document) = self.documents.get(uri)
            && !document.syntax_diagnostics.is_empty()
        {
            return document.syntax_diagnostics.clone();
        }
        path.and_then(|path| self.check_diagnostics.get(path))
            .cloned()
            .unwrap_or_default()
    }

    fn publish(&mut self, uri: String, path: Option<&Path>) -> Option<Value> {
        let diagnostics = self.diagnostics_for(&uri, path);
        if diagnostics.is_empty() && !self.shown.remove(&uri) {
            return None;
        }
        if !diagnostics.is_empty() {
            self.shown.insert(uri.clone());
        }
        Some(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": diagnostics },
        }))
    }

    fn publish_path(&mut self, path: &Path) -> Option<Value> {
        // Prefer the client's own spelling of an open file's URI.
        let uri = self
            .documents
            .values()
            .find(|document| document.path.as_deref() == Some(path))
            .map_or_else(|| path_to_uri(path), |document| document.uri.clone());
        self.publish(uri, Some(path))
    }

    fn publish_for_document(&mut self, uri: &str) -> Vec<Value> {
        let path = uri_to_path(uri);
        self.publish(uri.to_owned(), path.as_deref())
            .into_iter()
            .collect()
    }

    // ---------------------------------------------------------------------
    // Navigation

    /// Every module the server knows: open buffers first (the requesting
    /// buffer before the others), then saved files of retained programs that
    /// are not open.
    fn modules<'a>(&'a self, first: Option<&str>) -> Vec<&'a ModuleIndex> {
        let mut documents: Vec<&Document> = self.documents.values().collect();
        documents.sort_by_key(|document| (Some(document.uri.as_str()) != first, &document.uri));
        let open: HashSet<&Path> = documents
            .iter()
            .filter_map(|document| document.path.as_deref())
            .collect();
        let mut modules: Vec<&ModuleIndex> = documents
            .iter()
            .filter_map(|document| document.index.as_ref())
            .collect();
        let mut seen: HashSet<&Path> = HashSet::new();
        if let Some(checker) = &self.checker {
            for module in checker.modules() {
                if !open.contains(module.path.as_path()) && seen.insert(module.path.as_path()) {
                    modules.push(module);
                }
            }
        }
        modules
    }

    /// The URI a module is reported under: the client's spelling for an open
    /// buffer, the file's own URI otherwise.
    fn uri_for(&self, module: &ModuleIndex) -> String {
        self.documents
            .values()
            .find(|document| {
                document
                    .index
                    .as_ref()
                    .is_some_and(|index| std::ptr::eq(index, module))
            })
            .map_or_else(
                || path_to_uri(&module.path),
                |document| document.uri.clone(),
            )
    }

    /// The requesting buffer's index and the byte offset of the request's
    /// position in it.
    fn position<'a>(&'a self, params: &Value) -> Option<(&'a str, &'a ModuleIndex, usize)> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let document = self.documents.get(uri)?;
        let index = document.index.as_ref()?;
        let position = Position::from_json(&params["position"])?;
        Some((
            document.uri.as_str(),
            index,
            index.lines.offset(&index.text, position),
        ))
    }

    /// Resolve the identifier at the request position to its declarations.
    fn resolve<'a>(&'a self, params: &Value) -> Vec<(&'a ModuleIndex, Definition)> {
        let Some((uri, index, offset)) = self.position(params) else {
            return Vec::new();
        };
        let Some((_, name, reference)) = index.identifier_at(offset) else {
            return Vec::new();
        };
        self.resolve_name(uri, index, offset, name, reference)
    }

    fn resolve_name<'a>(
        &'a self,
        uri: &str,
        index: &'a ModuleIndex,
        offset: usize,
        name: &str,
        reference: Reference,
    ) -> Vec<(&'a ModuleIndex, Definition)> {
        let token = index.token_at(offset);
        if reference == Reference::Member
            && let Some(alias) = token.and_then(|token| index.receiver_of(token))
            && index.local_at(offset, alias).is_none()
            && let Some(spec) = index.import_alias(alias)
        {
            // `module.item` through `const module = @import("spec")`: the
            // item is a module-level declaration of the imported file.
            let target = index
                .path
                .parent()
                .map(|directory| normalize(&directory.join(spec)));
            let imported = self
                .modules(Some(uri))
                .into_iter()
                .find(|module| Some(&module.path) == target.as_ref());
            if let Some(module) = imported {
                return module
                    .declarations_named(name, Reference::Name)
                    .into_iter()
                    .map(|definition| (module, definition))
                    .collect();
            }
        }
        if reference == Reference::Name {
            if let Some(local) = index.local_at(offset, name) {
                return vec![(index, local)];
            }
            let own = index.declarations_named(name, reference);
            if !own.is_empty() {
                return own
                    .into_iter()
                    .map(|definition| (index, definition))
                    .collect();
            }
        }
        self.modules(Some(uri))
            .into_iter()
            .flat_map(|module| {
                module
                    .declarations_named(name, reference)
                    .into_iter()
                    .map(move |definition| (module, definition))
            })
            .collect()
    }

    fn definition(&self, params: &Value) -> Option<Value> {
        let found = self.resolve(params);
        if found.is_empty() {
            return None;
        }
        Some(Value::Array(
            found
                .iter()
                .map(|(module, definition)| features::location(module, definition))
                .collect(),
        ))
    }

    fn hover(&self, params: &Value) -> Option<Value> {
        let found = self.resolve(params);
        let (module, definition) = found.first()?;
        let (_, index, offset) = self.position(params)?;
        let token = &index.tokens[index.token_at(offset)?];
        Some(json!({
            "contents": { "kind": "markdown", "value": features::hover(module, definition) },
            "range": features::range_of(index, token.start, token.end).to_json(),
        }))
    }

    /// Occurrences of the identifier at the request position that resolve
    /// to the same declaration. References search every known module;
    /// highlights stay within the requesting buffer.
    fn references(&self, params: &Value, workspace: bool) -> Option<Value> {
        let (uri, index, offset) = self.position(params)?;
        let (_, name, reference) = index.identifier_at(offset)?;
        let targets: Vec<(std::path::PathBuf, usize)> = self
            .resolve_name(uri, index, offset, name, reference)
            .into_iter()
            .map(|(module, definition)| (module.path.clone(), definition.name_start))
            .collect();
        if targets.is_empty() {
            return None;
        }
        let include_declaration = params["context"]["includeDeclaration"]
            .as_bool()
            .unwrap_or(true);
        let modules = if workspace {
            self.modules(Some(uri))
        } else {
            vec![index]
        };
        let mut found = Vec::new();
        for module in modules {
            let module_uri = self.uri_for(module);
            for token in &module.tokens {
                if token.kind != "IDENT" || token.value.as_deref() != Some(name) {
                    continue;
                }
                let Some((_, _, role)) = module.identifier_at(token.start) else {
                    continue;
                };
                let resolved = self.resolve_name(&module_uri, module, token.start, name, role);
                let is_target = resolved.iter().any(|(owner, definition)| {
                    targets.contains(&(owner.path.clone(), definition.name_start))
                });
                if !is_target {
                    continue;
                }
                let is_declaration = targets.contains(&(module.path.clone(), token.start));
                if is_declaration && !include_declaration && workspace {
                    continue;
                }
                let range = features::range_of(module, token.start, token.end).to_json();
                found.push(if workspace {
                    json!({ "uri": module_uri, "range": range })
                } else {
                    json!({ "range": range, "kind": if is_declaration { 3 } else { 2 } })
                });
            }
        }
        Some(Value::Array(found))
    }

    fn document_symbols(&self, params: &Value) -> Option<Value> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let index = self.documents.get(uri)?.index.as_ref()?;
        Some(Value::Array(features::document_symbols(index)))
    }

    fn completion(&self, params: &Value) -> Option<Value> {
        let (_, index, offset) = self.position(params)?;
        let uri = params["textDocument"]["uri"].as_str()?;
        let text = &self.documents.get(uri)?.text;
        // The trigger is judged on the live text: the index may predate the
        // `.` the user just typed.
        let live = uri_offset(text, &params["position"]);
        let after_dot = text[..live]
            .trim_end_matches(|ch: char| ch.is_alphanumeric() || ch == '_')
            .ends_with('.');
        Some(Value::Array(features::completions(
            index, offset, after_dot,
        )))
    }

    fn semantic_tokens(&self, params: &Value) -> Option<Value> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let index = self.documents.get(uri)?.index.as_ref()?;
        Some(json!({ "data": features::semantic_tokens(index) }))
    }

    fn workspace_symbols(&self, params: &Value) -> Value {
        let query = params["query"].as_str().unwrap_or_default().to_lowercase();
        let mut symbols = Vec::new();
        for module in self.modules(None) {
            let uri = self.uri_for(module);
            for definition in module.declarations() {
                if !definition.name.to_lowercase().contains(&query) {
                    continue;
                }
                let mut symbol = json!({
                    "name": definition.name,
                    "kind": features::symbol_kind(definition.kind),
                    "location": {
                        "uri": uri,
                        "range": features::range_of(module, definition.name_start, definition.name_end).to_json(),
                    },
                });
                if let Some(container) = &definition.container {
                    symbol["containerName"] = json!(container);
                }
                symbols.push(symbol);
            }
        }
        Value::Array(symbols)
    }
}

/// Lexically normalize `.` and `..` components, as the compiler host
/// resolves import paths.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

fn uri_offset(text: &str, position: &Value) -> usize {
    let lines = crate::text::LineIndex::new(text);
    Position::from_json(position).map_or(0, |position| lines.offset(text, position))
}

/// Quick fixes from the compiler's own suggestions, which the server
/// attached to each diagnostic it published.
fn code_actions(params: &Value) -> Value {
    let mut actions = Vec::new();
    for diagnostic in params["context"]["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for suggestion in diagnostic["data"]["suggestions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let (Some(uri), Some(message)) =
                (suggestion["uri"].as_str(), suggestion["message"].as_str())
            else {
                continue;
            };
            let preferred = suggestion["applicability"]
                .as_str()
                .is_some_and(|applicability| {
                    applicability.to_ascii_lowercase().contains("machine")
                });
            actions.push(json!({
                "title": message,
                "kind": "quickfix",
                "diagnostics": [diagnostic],
                "isPreferred": preferred,
                "edit": {
                    "changes": {
                        uri: [{ "range": suggestion["range"], "newText": suggestion["newText"] }],
                    },
                },
            }));
        }
    }
    Value::Array(actions)
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Session {
        server: Server,
        dir: tempfile::TempDir,
        next_id: i64,
    }

    impl Session {
        fn new(files: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (name, text) in files {
                std::fs::write(dir.path().join(name), text).unwrap();
            }
            let mut session = Self {
                server: Server::new(),
                dir,
                next_id: 0,
            };
            let workspace = path_to_uri(&session.path(""));
            let response = session.request(
                "initialize",
                json!({ "rootUri": workspace, "capabilities": {} }),
            );
            assert_eq!(
                response["result"]["capabilities"]["textDocumentSync"]["change"],
                1
            );
            session.notify("initialized", json!({}));
            session
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().canonicalize().unwrap().join(name)
        }

        fn uri(&self, name: &str) -> String {
            path_to_uri(&self.path(name))
        }

        fn request(&mut self, method: &str, params: Value) -> Value {
            self.next_id += 1;
            let mut outgoing = self.server.handle(json!({
                "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params,
            }));
            assert_eq!(outgoing.len(), 1, "{outgoing:#?}");
            let response = outgoing.remove(0);
            assert_eq!(response["id"], self.next_id);
            response
        }

        fn notify(&mut self, method: &str, params: Value) -> Vec<Value> {
            self.server
                .handle(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
        }

        fn open(&mut self, name: &str) -> Vec<Value> {
            let text = std::fs::read_to_string(self.path(name)).unwrap();
            let uri = self.uri(name);
            self.notify(
                "textDocument/didOpen",
                json!({ "textDocument": {
                    "uri": uri, "languageId": "rue", "version": 1, "text": text,
                } }),
            )
        }

        fn at(&self, name: &str, needle: &str) -> Value {
            let text = std::fs::read_to_string(self.path(name)).unwrap();
            let offset = text.find(needle).unwrap();
            let position = crate::text::LineIndex::new(&text).position(&text, offset);
            json!({ "textDocument": { "uri": self.uri(name) }, "position": position.to_json() })
        }
    }

    fn published<'a>(messages: &'a [Value], uri: &str) -> Option<&'a Vec<Value>> {
        messages
            .iter()
            .filter(|message| message["method"] == "textDocument/publishDiagnostics")
            .find(|message| message["params"]["uri"] == uri)
            .and_then(|message| message["params"]["diagnostics"].as_array())
    }

    const MAIN: &str = "\
const helper = @import(\"helper.rue\");

/// Doubles a value.
fn double(value: i32) -> i32 {
    value * 2
}

fn main() -> i32 {
    let base = helper.seed();
    double(base)
}
";
    const HELPER: &str = "pub fn seed() -> i32 { 21 }\n";

    #[test]
    fn requests_before_initialize_are_rejected() {
        let mut server = Server::new();
        let response = server.handle(json!({
            "jsonrpc": "2.0", "id": 1, "method": "textDocument/hover", "params": {},
        }));
        assert_eq!(response[0]["error"]["code"], SERVER_NOT_INITIALIZED);
    }

    #[test]
    fn a_clean_program_publishes_nothing_and_a_broken_save_publishes_errors() {
        let mut session = Session::new(&[("main.rue", MAIN), ("helper.rue", HELPER)]);
        let opened = session.open("main.rue");
        assert!(
            opened
                .iter()
                .all(|message| message["method"] != "textDocument/publishDiagnostics"),
            "{opened:#?}"
        );
        std::fs::write(
            session.path("helper.rue"),
            "pub fn seed() -> i32 { true }\n",
        )
        .unwrap();
        let uri = session.uri("main.rue");
        let saved = session.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        );
        let helper = published(&saved, &session.uri("helper.rue")).expect("helper diagnostics");
        assert_eq!(helper.len(), 1);
        assert_eq!(helper[0]["source"], "rue");
        // Fixing the file clears what was shown.
        std::fs::write(session.path("helper.rue"), HELPER).unwrap();
        let saved = session.notify(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        );
        assert_eq!(
            published(&saved, &session.uri("helper.rue")),
            Some(&Vec::new())
        );
    }

    #[test]
    fn a_syntax_error_while_typing_replaces_the_saved_analysis_until_it_parses() {
        let mut session = Session::new(&[("main.rue", MAIN), ("helper.rue", HELPER)]);
        session.open("main.rue");
        let uri = session.uri("main.rue");
        let changed = session.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "fn main() -> i32 { 0 " }],
            }),
        );
        assert!(!published(&changed, &uri).unwrap().is_empty());
        let fixed = session.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": 3 },
                "contentChanges": [{ "text": "fn main() -> i32 { 0 }" }],
            }),
        );
        assert_eq!(published(&fixed, &uri), Some(&Vec::new()));
    }

    #[test]
    fn navigation_resolves_locals_items_and_imported_modules() {
        let mut session = Session::new(&[("main.rue", MAIN), ("helper.rue", HELPER)]);
        session.open("main.rue");

        let definition = session.request(
            "textDocument/definition",
            session.at("main.rue", "double(base)"),
        );
        let location = &definition["result"][0];
        assert_eq!(location["uri"], session.uri("main.rue"));
        assert_eq!(location["range"]["start"]["line"], 3);

        let local = session.request("textDocument/definition", session.at("main.rue", "base)"));
        assert_eq!(local["result"][0]["range"]["start"]["line"], 8);

        // `seed` lives in the imported module, which the check indexed.
        let member = session.request("textDocument/definition", session.at("main.rue", "seed()"));
        assert_eq!(member["result"][0]["uri"], session.uri("helper.rue"));

        let hover = session.request("textDocument/hover", session.at("main.rue", "double(base)"));
        let contents = hover["result"]["contents"]["value"].as_str().unwrap();
        assert!(
            contents.contains("fn double(value: i32) -> i32"),
            "{contents}"
        );
        assert!(contents.contains("Doubles a value."), "{contents}");

        let references = session.request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": session.uri("main.rue") },
                "position": session.at("main.rue", "value * 2")["position"],
                "context": { "includeDeclaration": true },
            }),
        );
        assert_eq!(references["result"].as_array().unwrap().len(), 2);

        let symbols = session.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": session.uri("main.rue") } }),
        );
        let names: Vec<&str> = symbols["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["helper", "double", "main"]);

        let workspace = session.request("workspace/symbol", json!({ "query": "SEE" }));
        assert_eq!(workspace["result"][0]["name"], "seed");
    }

    #[test]
    fn a_module_without_main_is_checked_through_the_enclosing_program() {
        let mut session = Session::new(&[
            ("main.rue", MAIN),
            ("helper.rue", "pub fn seed() -> i32 { true }\n"),
        ]);
        let opened = session.open("helper.rue");
        let helper = published(&opened, &session.uri("helper.rue")).expect("diagnostics");
        assert_eq!(helper.len(), 1, "{helper:#?}");
    }

    #[test]
    fn compiler_suggestions_become_quick_fixes() {
        let diagnostic = json!({
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "message": "m",
            "data": { "suggestions": [{
                "message": "use `y`",
                "uri": "file:///a.rue",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
                "newText": "y",
                "applicability": "MachineApplicable",
            }] },
        });
        let actions = code_actions(&json!({ "context": { "diagnostics": [diagnostic] } }));
        assert_eq!(actions[0]["title"], "use `y`");
        assert_eq!(actions[0]["isPreferred"], true);
        assert_eq!(
            actions[0]["edit"]["changes"]["file:///a.rue"][0]["newText"],
            "y"
        );
    }

    #[test]
    fn shutdown_then_exit_is_a_clean_exit() {
        let mut session = Session::new(&[]);
        assert_eq!(
            session.request("shutdown", Value::Null)["result"],
            Value::Null
        );
        assert_eq!(
            session.request("textDocument/hover", json!({}))["error"]["code"],
            INVALID_REQUEST
        );
        session.notify("exit", Value::Null);
        assert_eq!(session.server.exit_code(), Some(0));
    }
}
