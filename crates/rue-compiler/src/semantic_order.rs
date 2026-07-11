//! Canonical ordering for semantic lowering.
//!
//! Caller source order remains observable in tokens, AST, RIR, and dependency
//! output. Semantic allocation has a stricter requirement:
//! moving sibling modules must not renumber types, strings, functions, or
//! objects. The compiler therefore lowers an immutable, borrowed item-order
//! view to the private RIR consumed by sema.

use std::collections::HashMap;

use rue_parser::{Ast, Item};
use rue_span::{FileId, Span};

use crate::SourceMetadata;

pub(crate) fn item_span(item: &Item) -> Span {
    match item {
        Item::Function(item) => item.span,
        Item::Struct(item) => item.span,
        Item::Enum(item) => item.span,
        Item::DropFn(item) => item.span,
        Item::Const(item) => item.span,
        Item::Error(span) => *span,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SemanticItemOrderWork {
    pub(crate) items_indexed: usize,
    pub(crate) item_payloads_cloned: usize,
}

/// Borrowed canonical item order used only by semantic lowering.
pub(crate) struct SemanticItemOrder<'a> {
    items: Vec<&'a Item>,
    work: SemanticItemOrderWork,
}

impl<'a> SemanticItemOrder<'a> {
    /// Index an observable AST without cloning any item payloads.
    ///
    /// The explicitly designated root remains first. Siblings are ordered by their
    /// validated, canonical logical paths, while byte positions retain
    /// declaration order within each file. Compiler entry points validate that
    /// every AST item is described by `source_metadata` before calling this.
    pub(crate) fn from_items(
        items: impl IntoIterator<Item = &'a Item>,
        source_metadata: &SourceMetadata,
    ) -> Self {
        let mut keyed_items: Vec<_> = items
            .into_iter()
            .enumerate()
            .map(|(original_index, item)| {
                let span = item_span(item);
                let logical_path = source_metadata
                    .logical_path(span.file_id)
                    .expect("AST file ID validated against source metadata");
                let key = (
                    span.file_id != source_metadata.root_file_id(),
                    logical_path,
                    span.start,
                    span.end,
                    span.file_id.index(),
                    original_index,
                );
                (key, item)
            })
            .collect();
        let keyed_items_len = keyed_items.len();
        keyed_items.sort_by(|(left, _), (right, _)| left.cmp(right));

        Self {
            items: keyed_items.into_iter().map(|(_, item)| item).collect(),
            work: SemanticItemOrderWork {
                items_indexed: keyed_items_len,
                item_payloads_cloned: 0,
            },
        }
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = &'a Item> + '_ {
        self.items.iter().copied()
    }

    pub(crate) fn work(&self) -> SemanticItemOrderWork {
        self.work
    }
}

/// Recover the historical minimum-FileId root for compatibility entry points.
///
/// New multi-file orchestration must pass the designated root explicitly.
pub(crate) fn legacy_root_file_id(ast: &Ast, file_paths: &HashMap<FileId, String>) -> FileId {
    file_paths
        .keys()
        .copied()
        .min_by_key(|file_id| file_id.index())
        .or_else(|| {
            ast.items
                .iter()
                .map(item_span)
                .map(|span| span.file_id)
                .min_by_key(|file_id| file_id.index())
        })
        .unwrap_or(FileId::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_explicit_root_and_normalized_logical_paths() {
        let root = FileId::new(30);
        let logical_first = FileId::new(20);
        let logical_second = FileId::new(10);
        let ast = Ast {
            items: vec![
                Item::Error(Span::with_file(logical_second, 0, 1)),
                Item::Error(Span::with_file(logical_first, 0, 1)),
                Item::Error(Span::with_file(root, 0, 1)),
            ],
        };
        let source_metadata = SourceMetadata::new(
            root,
            HashMap::from([
                (root, "z-root.rue".to_string()),
                (logical_first, "z-physical.rue".to_string()),
                (logical_second, "a-physical.rue".to_string()),
            ]),
            HashMap::from([
                (root, "z-root.rue".to_string()),
                (logical_first, "z/../a.rue".to_string()),
                (logical_second, "b.rue".to_string()),
            ]),
        )
        .unwrap();

        let ordered = SemanticItemOrder::from_items(ast.items.iter(), &source_metadata);
        let ordered_files: Vec<_> = ordered.iter().map(item_span).map(|s| s.file_id).collect();

        assert_eq!(ordered_files, [root, logical_first, logical_second]);
        assert_eq!(
            ordered.work(),
            SemanticItemOrderWork {
                items_indexed: 3,
                item_payloads_cloned: 0,
            }
        );
        for item in ordered.iter() {
            assert!(ast.items.iter().any(|source| std::ptr::eq(source, item)));
        }
    }
}
