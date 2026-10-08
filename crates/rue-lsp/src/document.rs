//! Open editor buffers.
//!
//! Each buffer owns a `CompilerSession` over a one-file snapshot of its live
//! text. Publishing a new revision reparses only that buffer and yields the
//! canonical syntax view the navigation features read, plus the parser's own
//! diagnostics while the buffer does not parse. Whole-program analysis reads
//! saved files through the retained host in [`crate::check`]; the buffer
//! session never resolves imports or analyzes semantics.

use std::path::PathBuf;
use std::sync::Arc;

use ahash::AHashMap;
use rue_compiler::{
    CompilerSession, CompilerSessionConfig, FileId, SourceMetadata, SourceSnapshot,
};
use serde_json::Value;

use crate::check::buffer_diagnostics;
use crate::syntax_index::ModuleIndex;
use crate::text::uri_to_path;

/// A buffer session parses one file; it needs no worker pool and retains
/// only that file's artifacts.
const BUFFER_RETAINED_BYTE_BUDGET: u64 = 16 * 1024 * 1024;
const BUFFER_DEPENDENCY_PIN_BUDGET: u64 = 100_000;

pub struct Document {
    pub uri: String,
    /// The file a `file:` URI names. Other schemes (unsaved buffers) still
    /// get syntax features but never take part in a whole-program check.
    pub path: Option<PathBuf>,
    pub text: String,
    session: CompilerSession,
    /// Navigation records from the most recent revision that parsed. While
    /// the buffer has a syntax error this is the last good parse, so outline
    /// and navigation keep working mid-edit.
    pub index: Option<ModuleIndex>,
    /// Parser diagnostics of the current revision; empty when it parses.
    pub syntax_diagnostics: Vec<Value>,
}

impl Document {
    pub fn open(uri: String, text: String) -> Self {
        let configuration = CompilerSessionConfig::with_workers_and_retention(
            1,
            BUFFER_RETAINED_BYTE_BUDGET,
            BUFFER_DEPENDENCY_PIN_BUDGET,
        )
        .expect("the buffer session's resource policy is valid");
        let mut document = Self {
            path: uri_to_path(&uri),
            uri,
            text: String::new(),
            session: CompilerSession::with_configuration(configuration),
            index: None,
            syntax_diagnostics: Vec::new(),
        };
        document.update(text);
        document
    }

    pub fn update(&mut self, text: String) {
        self.text = text;
        let spelling = self.path.as_ref().map_or_else(
            || self.uri.clone(),
            |path| path.to_string_lossy().into_owned(),
        );
        let file = FileId::new(1);
        let paths = AHashMap::from([(file, spelling.clone())]);
        let snapshot = SourceMetadata::new(file, paths.clone(), paths).and_then(|metadata| {
            SourceSnapshot::new(metadata, vec![(file, Arc::new(self.text.clone()))])
        });
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            // The only rejection of a one-file snapshot is a source the
            // compiler refuses outright (for example one over its size
            // limit); the buffer then simply has no syntax features.
            Err(_) => {
                self.index = None;
                self.syntax_diagnostics.clear();
                return;
            }
        };
        match self.session.update(&snapshot).into_result() {
            Ok(syntax) => {
                let path = PathBuf::from(&spelling);
                self.index = syntax
                    .modules()
                    .next()
                    .map(|module| ModuleIndex::new(&module, path));
                self.syntax_diagnostics.clear();
            }
            Err(errors) => {
                self.syntax_diagnostics = buffer_diagnostics(&snapshot, &errors);
            }
        }
    }

    /// Whether the last good parse declares `fn main`, making this file the
    /// root of its own program.
    pub fn declares_main(&self) -> bool {
        self.index.as_ref().is_some_and(|index| {
            index
                .items
                .iter()
                .any(|item| item.kind == "function" && item.name.as_deref() == Some("main"))
        })
    }

    /// Whether the last good parse declares any `test` item.
    pub fn declares_tests(&self) -> bool {
        self.index
            .as_ref()
            .is_some_and(|index| index.items.iter().any(|item| item.kind == "test"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_syntax_error_keeps_the_last_good_parse() {
        let mut document = Document::open(
            "file:///tmp/main.rue".into(),
            "fn main() -> i32 { 0 }\n".into(),
        );
        assert!(document.syntax_diagnostics.is_empty());
        assert!(document.declares_main());
        document.update("fn main() -> i32 { 0 \n".into());
        assert!(!document.syntax_diagnostics.is_empty());
        let diagnostic = &document.syntax_diagnostics[0];
        assert_eq!(diagnostic["severity"], 1);
        assert!(document.declares_main(), "the last good parse is retained");
        document.update("fn helper() -> i32 { 0 }\n".into());
        assert!(document.syntax_diagnostics.is_empty());
        assert!(!document.declares_main());
    }

    #[test]
    fn a_buffer_with_imports_parses_without_reading_them() {
        let document = Document::open(
            "untitled:Untitled-1".into(),
            "const helper = @import(\"missing.rue\");\nfn main() -> i32 { helper.value() }\n"
                .into(),
        );
        assert_eq!(document.path, None);
        assert!(document.syntax_diagnostics.is_empty());
        assert!(document.index.is_some());
    }
}
