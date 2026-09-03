//! The one place a test closure's ordered inventory is computed (ADR-0083 §2).
//!
//! Two consumers read this order, and they must never disagree: the `--list`
//! surface a runner publishes, and the ordinal each test is dispatched by
//! inside the test image. A selector is an index into this table, so two
//! independent sorts would silently run the wrong body. Both therefore call
//! [`collect_test_inventory`], and the dispatcher's table is built from its
//! result rather than from a second pass over the same declarations.
//!
//! The stable ID is `<module>::<name>`. `<module>` is the module's published
//! identity — project-root-relative for user modules; a standard-library module
//! reports its requested path instead of the internal trusted spelling, the
//! same guard `import_discovery` applies when it renders a module path for a
//! consumer. `<name>` is the test's string literal verbatim, which may contain
//! spaces and punctuation and is never mangled here (the linker symbol, which
//! cannot carry those bytes, is a separate concern with its own digest).
//!
//! Ordering is byte order over the whole ID. It is deliberately not
//! declaration order, module order, or symbol order: only a property of the
//! source text itself keeps a filtered run's ordinals equal to a full run's.

use std::sync::Arc;

use crate::parsed_modules::ParsedModule;
use crate::unstable::TestInventoryEntry;

/// One inventoried test: what a consumer is shown, and what the image calls.
///
/// The two travel together so a consumer of the ordering never has to recover
/// an identity from a rendered ID. Recovering it would have to parse a display
/// path back into a `ModuleId`, and a standard-library module's display path is
/// deliberately not its `ModuleId`.
#[derive(Debug, Clone)]
pub(crate) struct RootedTest {
    pub(crate) entry: TestInventoryEntry,
    pub(crate) identity: crate::FunctionInstanceKey,
}

/// Build the ordered inventory for a request's rooted test declarations.
///
/// `roots` is the request's root set exactly as the root-set authority
/// published it; every `StableDefinitionKind::Test` definition in it becomes
/// one entry, and nothing else does — so an executable request, whose root set
/// holds no tests, yields an empty inventory without a special case.
/// `modules` supplies the source locations, which the semantic identities do
/// not carry.
///
/// A test whose declaration cannot be located in its module's AST still gets an
/// entry, at line and column zero. Dropping it would renumber every later
/// ordinal and silently dispatch the wrong body; the inventory's job is to be
/// complete and stably ordered, not to diagnose.
pub(crate) fn collect_test_inventory(
    modules: &[Arc<ParsedModule>],
    roots: &[crate::FunctionInstanceKey],
) -> Vec<RootedTest> {
    let mut entries = Vec::new();
    for root in roots {
        let crate::FunctionInstanceKey::Definition(key) = root else {
            continue;
        };
        if key.kind() != crate::StableDefinitionKind::Test {
            continue;
        }
        let module = modules
            .iter()
            .find(|module| module.module_id() == key.module());
        let name = key.name().to_owned();
        let module_path = match module {
            Some(module) => module_display_path(module),
            None => key.module().as_str().to_owned(),
        };
        let (file, line, column) = match module {
            Some(module) => {
                let file = module.physical_path().to_owned();
                match declaration_header_start(module, &name) {
                    Some(offset) => {
                        let index = rue_span::LineIndex::new(module.source_text());
                        let (line, column) = index.line_col(offset);
                        (file, line, column)
                    }
                    None => (file, 0, 0),
                }
            }
            None => (String::new(), 0, 0),
        };
        entries.push(RootedTest {
            entry: TestInventoryEntry {
                id: format!("{module_path}::{name}"),
                module: module_path,
                name,
                file,
                line,
                column,
                ordinal: 0,
            },
            identity: root.clone(),
        });
    }
    entries.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
    for (ordinal, test) in entries.iter_mut().enumerate() {
        test.entry.ordinal = u32::try_from(ordinal).unwrap_or(u32::MAX);
    }
    entries
}

/// The module identity a consumer is shown.
///
/// A standard-library module's `ModuleId` carries the internal trusted
/// namespace, which is not a path anyone can type. Reporting its requested path
/// keeps a `std` test's ID and a user test's ID the same kind of thing.
fn module_display_path(module: &ParsedModule) -> String {
    if module.module_id().is_trusted_standard_library() {
        module.physical_path().to_owned()
    } else {
        module.module_id().as_str().to_owned()
    }
}

/// Byte offset of `test "name"` in its module's source.
///
/// The header span rather than the whole declaration: a failure points a reader
/// at the test that failed, not at the first byte of a body that may be pages
/// long. Names are unique within a module (a duplicate is a compile error), so
/// the first match is the only match.
fn declaration_header_start(module: &ParsedModule, name: &str) -> Option<u32> {
    module.ast().items.iter().find_map(|item| match item {
        rue_parser::ast::Item::Test(test) => (module.try_resolve_raw_symbol(test.name.value)
            == Some(name))
        .then_some(test.header_span.start),
        _ => None,
    })
}
