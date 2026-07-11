//! Canonical ordering for semantic lowering.
//!
//! Caller source order remains observable in tokens, AST, RIR, and dependency
//! output. Semantic allocation has a stricter requirement:
//! moving sibling modules must not renumber types, strings, functions, or
//! objects. The compiler therefore lowers a second, top-level-item-reordered
//! AST to the private RIR consumed by sema.

use std::collections::HashMap;

use rue_air::normalize_module_path;
use rue_parser::{Ast, Item};
use rue_span::{FileId, Span};

fn item_span(item: &Item) -> Span {
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
/// relocation-stable logical paths, while byte positions retain declaration
/// order within each file. The numeric fallbacks only serve legacy embedders
/// that do not provide a complete symbol-path map.
pub(crate) fn for_sema(
    ast: &Ast,
    file_paths: &HashMap<FileId, String>,
    symbol_paths: &HashMap<FileId, String>,
    root_file_id: FileId,
) -> Ast {
    let mut keyed_items: Vec<_> = ast
        .items
        .iter()
        .cloned()
        .enumerate()
        .map(|(original_index, item)| {
            let span = item_span(&item);
            let logical_path = symbol_paths
                .get(&span.file_id)
                .or_else(|| file_paths.get(&span.file_id))
                .map(|path| normalize_module_path(path))
                .unwrap_or_else(|| format!("file{:010}", span.file_id.index()));
            let key = (
                span.file_id != root_file_id,
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
    fn normalizes_effective_paths_from_a_partial_symbol_map() {
        let logical = FileId::new(20);
        let physical_fallback = FileId::new(10);
        let ast = Ast {
            items: vec![
                Item::Error(Span::with_file(logical, 0, 1)),
                Item::Error(Span::with_file(physical_fallback, 0, 1)),
            ],
        };
        let file_paths = HashMap::from([
            (logical, "unused-physical.rue".to_string()),
            (physical_fallback, "b.rue".to_string()),
        ]);
        let symbol_paths = HashMap::from([(logical, "z/../a.rue".to_string())]);

        let ordered = for_sema(&ast, &file_paths, &symbol_paths, FileId::new(99));
        let ordered_files: Vec<_> = ordered
            .items
            .iter()
            .map(item_span)
            .map(|s| s.file_id)
            .collect();

        assert_eq!(ordered_files, [logical, physical_fallback]);
    }
}
