//! Canonical ordering for semantic lowering.
//!
//! Caller source order remains observable in tokens, AST, RIR, and dependency
//! output. Semantic allocation has a stricter requirement:
//! moving sibling modules must not renumber types, strings, functions, or
//! objects. The compiler therefore lowers a second, top-level-item-reordered
//! AST to the private RIR consumed by sema.

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

/// Clone an observable AST into the canonical order used only by sema.
///
/// The explicitly designated root remains first. Siblings are ordered by their
/// validated, canonical logical paths, while byte positions retain
/// declaration order within each file. Compiler entry points validate that
/// every AST item is described by `source_metadata` before calling this.
pub(crate) fn for_sema(ast: &Ast, source_metadata: &SourceMetadata) -> Ast {
    let mut keyed_items: Vec<_> = ast
        .items
        .iter()
        .cloned()
        .enumerate()
        .map(|(original_index, item)| {
            let span = item_span(&item);
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
    keyed_items.sort_by(|(left, _), (right, _)| left.cmp(right));

    Ast {
        items: keyed_items.into_iter().map(|(_, item)| item).collect(),
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

        let ordered = for_sema(&ast, &source_metadata);
        let ordered_files: Vec<_> = ordered
            .items
            .iter()
            .map(item_span)
            .map(|s| s.file_id)
            .collect();

        assert_eq!(ordered_files, [root, logical_first, logical_second]);
    }
}
