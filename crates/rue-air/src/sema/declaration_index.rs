//! Snapshot-local lookup tables for raw RIR declaration candidates.
//!
//! This index is an implementation detail of one semantic epoch's RIR view.
//! Its [`InstRef`] values, [`FileId`] values, and [`Spur`] values are meaningful
//! only with the exact RIR and interner epoch from which it was built. They are
//! arena locators, not durable semantic or tooling identities.

use ahash::{AHashMap, AHashSet};

use lasso::Spur;
use rue_rir::{InstData, InstRef, Rir};
use rue_span::{FileId, Span};

/// Exact structural work performed while building one RIR declaration index.
///
/// These counters make index construction measurable without exposing any of
/// its request-local instruction handles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RirDeclarationIndexWork {
    /// Number of index builds represented by this value.
    pub build_invocations: usize,
    /// Number of entries visited through [`Rir::iter`].
    pub rir_instructions_visited: usize,
    /// Number of method-owner edges decoded from named and anonymous structs.
    pub method_references_visited: usize,
    /// Number of true free-function candidates retained.
    pub free_functions_indexed: usize,
    /// Number of functions owned by named structs.
    pub named_methods_indexed: usize,
    /// Number of functions owned by anonymous structs.
    pub anonymous_methods_indexed: usize,
    /// Number of named destructor declarations retained.
    pub destructors_indexed: usize,
    /// Number of syntactic constant candidates retained before semantic
    /// evaluation classifies value constants versus module bindings.
    pub const_candidates_indexed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RirDestructorDeclaration {
    pub(super) declaration: InstRef,
    pub(super) type_name: Spur,
    pub(super) body: InstRef,
    pub(super) span: Span,
}

#[derive(Debug, Clone, Copy)]
struct FunctionCandidate {
    declaration: InstRef,
    source_order: u32,
    file_id: FileId,
    name: Spur,
    has_self: bool,
    is_test: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RirShellDeclaration {
    pub(super) declaration: InstRef,
    pub(super) source_order: u32,
    pub(super) named_method_owner: Option<Spur>,
}

/// Immutable declaration candidates tied to one exact RIR arena.
///
/// Ordered vectors preserve RIR order, including duplicates in invalid input.
/// Hash tables are used only for point lookup; their iteration order never
/// drives diagnostics, allocation, or generated artifacts.
#[derive(Debug)]
pub(super) struct RirDeclarationIndex {
    free_functions: Vec<InstRef>,
    free_functions_by_file_name: AHashMap<(FileId, Spur), Vec<InstRef>>,
    free_functions_by_name: AHashMap<Spur, Vec<InstRef>>,
    /// Test declarations keyed by `(file, name)` (ADR-0083 §1). Disjoint from
    /// the free-function index: a test is not callable and never resolves a
    /// name, so the two key spaces cannot collide.
    tests_by_file_name: AHashMap<(FileId, Spur), Vec<InstRef>>,
    named_methods: Vec<InstRef>,
    anonymous_methods: Vec<InstRef>,
    destructors: Vec<RirDestructorDeclaration>,
    shell_declarations: Vec<RirShellDeclaration>,
    inline_ctor_head_candidates: usize,
    work: RirDeclarationIndexWork,
}

impl RirDeclarationIndex {
    pub(super) fn new(rir: &Rir) -> Self {
        let mut function_candidates = Vec::new();
        let mut named_method_refs = AHashSet::new();
        let mut named_method_owners = AHashMap::new();
        let mut anonymous_method_refs = AHashSet::new();
        let mut declaration_source_orders = AHashMap::new();
        let mut destructors = Vec::new();
        let mut const_shell_declarations = Vec::new();
        let mut work = RirDeclarationIndexWork {
            build_invocations: 1,
            ..RirDeclarationIndexWork::default()
        };
        let mut inline_ctor_head_candidates = 0_usize;

        // AstGen emits method FnDecls before their enclosing type. Retain all
        // function candidates during this single arena walk, collect owner
        // edges wherever their containers occur, then classify below.
        let mut nominal_candidates = Vec::new();
        for (source_order, (inst_ref, inst)) in rir.iter().enumerate() {
            work.rir_instructions_visited += 1;
            match &inst.data {
                InstData::FnDecl {
                    name,
                    has_self,
                    is_test,
                    ..
                } => {
                    function_candidates.push(FunctionCandidate {
                        declaration: inst_ref,
                        source_order: source_order as u32,
                        file_id: inst.span.file_id,
                        name: *name,
                        has_self: *has_self,
                        is_test: *is_test,
                    });
                }
                InstData::StructDecl { name, methods, .. } => {
                    for method_ref in rir.struct_methods(methods) {
                        work.method_references_visited += 1;
                        named_method_refs.insert(method_ref);
                        // A method may only have one named owner in valid RIR;
                        // retain the first edge so malformed input preserves
                        // the historical discovery semantics.
                        named_method_owners.entry(method_ref).or_insert(*name);
                    }
                    nominal_candidates.push((source_order as u32, inst_ref));
                }
                InstData::AnonStructType { methods, .. } => {
                    for method_ref in rir.anon_struct_methods(methods) {
                        work.method_references_visited += 1;
                        anonymous_method_refs.insert(method_ref);
                    }
                }
                InstData::DropFnDecl { type_name, body } => {
                    declaration_source_orders.insert(inst_ref, source_order as u32);
                    destructors.push(RirDestructorDeclaration {
                        declaration: inst_ref,
                        type_name: *type_name,
                        body: *body,
                        span: inst.span,
                    });
                }
                InstData::ConstDecl { .. } => {
                    const_shell_declarations.push(RirShellDeclaration {
                        declaration: inst_ref,
                        source_order: source_order as u32,
                        named_method_owner: None,
                    });
                }
                InstData::EnumDecl { .. } => {
                    nominal_candidates.push((source_order as u32, inst_ref));
                }
                _ => {}
            }
            // Whole-arena census of inline type-constructor head shapes
            // (RUE-596), taken during the walk this index already performs. A
            // zero census lets the inference precompute skip its per-body
            // reachability scan outright: the scan's candidates are a subset of
            // these arena occurrences, so zero here proves zero there.
            match &inst.data {
                InstData::MethodCall { receiver, .. } => {
                    if matches!(
                        rir.get(*receiver).data,
                        InstData::Call { .. } | InstData::MethodCall { .. }
                    ) {
                        inline_ctor_head_candidates += 1;
                    }
                }
                InstData::StructInit {
                    ctor_head: Some(_), ..
                } => {
                    inline_ctor_head_candidates += 1;
                }
                InstData::Match { arms, .. } => {
                    for (pattern, _) in rir.match_arms(arms).iter() {
                        if let rue_rir::RirPatternView::Path {
                            ctor_head: Some(_), ..
                        } = pattern
                        {
                            inline_ctor_head_candidates += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        let mut free_functions = Vec::new();
        let mut free_functions_by_file_name = AHashMap::<(FileId, Spur), Vec<InstRef>>::new();
        let mut free_functions_by_name = AHashMap::<Spur, Vec<InstRef>>::new();
        // Test declarations lower to `FnDecl`s but are NOT free functions: a
        // test's name is a string literal that no call can name (ADR-0083 §1),
        // so indexing one as a free function would both make it callable and
        // let `test "parse"` shadow `fn parse`. They get their own index.
        let mut tests_by_file_name = AHashMap::<(FileId, Spur), Vec<InstRef>>::new();
        let mut named_methods = Vec::new();
        let mut anonymous_methods = Vec::new();
        let mut shell_declarations = nominal_candidates
            .into_iter()
            .map(|(source_order, declaration)| RirShellDeclaration {
                declaration,
                source_order,
                named_method_owner: None,
            })
            .collect::<Vec<_>>();

        for candidate in function_candidates {
            if named_method_refs.contains(&candidate.declaration) {
                named_methods.push(candidate.declaration);
            } else if anonymous_method_refs.contains(&candidate.declaration) {
                anonymous_methods.push(candidate.declaration);
            } else if candidate.is_test {
                tests_by_file_name
                    .entry((candidate.file_id, candidate.name))
                    .or_default()
                    .push(candidate.declaration);
            } else if !candidate.has_self {
                free_functions.push(candidate.declaration);
                free_functions_by_file_name
                    .entry((candidate.file_id, candidate.name))
                    .or_default()
                    .push(candidate.declaration);
                free_functions_by_name
                    .entry(candidate.name)
                    .or_default()
                    .push(candidate.declaration);
            }
            if !anonymous_method_refs.contains(&candidate.declaration) {
                shell_declarations.push(RirShellDeclaration {
                    declaration: candidate.declaration,
                    source_order: candidate.source_order,
                    named_method_owner: named_method_owners.get(&candidate.declaration).copied(),
                });
            }
        }

        shell_declarations.extend(destructors.iter().map(|candidate| RirShellDeclaration {
            declaration: candidate.declaration,
            source_order: declaration_source_orders[&candidate.declaration],
            named_method_owner: None,
        }));
        work.const_candidates_indexed = const_shell_declarations.len();
        shell_declarations.extend(const_shell_declarations);
        shell_declarations.sort_by_key(|candidate| candidate.source_order);

        work.free_functions_indexed = free_functions.len();
        work.named_methods_indexed = named_methods.len();
        work.anonymous_methods_indexed = anonymous_methods.len();
        work.destructors_indexed = destructors.len();

        Self {
            free_functions,
            free_functions_by_file_name,
            free_functions_by_name,
            tests_by_file_name,
            named_methods,
            anonymous_methods,
            destructors,
            shell_declarations,
            inline_ctor_head_candidates,
            work,
        }
    }

    /// Whole-arena count of inline type-constructor head shapes (RUE-596):
    /// `.NAME(..)` receivers that are themselves calls, struct literals with an
    /// explicit constructor head, and match patterns carrying one. The
    /// inference precompute's reachability scan collects a subset of these
    /// occurrences, so a zero census proves that scan would find nothing.
    #[inline]
    pub(super) fn inline_ctor_head_candidates(&self) -> usize {
        self.inline_ctor_head_candidates
    }

    #[inline]
    pub(super) fn work(&self) -> RirDeclarationIndexWork {
        debug_assert_eq!(self.work.free_functions_indexed, self.free_functions.len());
        debug_assert_eq!(self.work.named_methods_indexed, self.named_methods.len());
        debug_assert_eq!(
            self.work.anonymous_methods_indexed,
            self.anonymous_methods.len()
        );
        debug_assert_eq!(self.work.destructors_indexed, self.destructors.len());
        self.work
    }

    /// The first test declaration named `name` in `file_id` (ADR-0083 §1).
    /// Test declarations live in their own index, disjoint from free
    /// functions, so a test and a function may share a spelling.
    pub(super) fn first_test(&self, name: Spur, file_id: FileId) -> Option<InstRef> {
        self.tests_by_file_name
            .get(&(file_id, name))
            .and_then(|candidates| candidates.first().copied())
    }

    pub(super) fn first_free_function(
        &self,
        name: Spur,
        file_id: Option<FileId>,
    ) -> Option<InstRef> {
        let candidates = match file_id {
            Some(file_id) => self.free_functions_by_file_name.get(&(file_id, name)),
            None => self.free_functions_by_name.get(&name),
        }?;
        candidates.first().copied()
    }

    pub(super) fn shell_declarations(&self) -> &[RirShellDeclaration] {
        &self.shell_declarations
    }

    #[cfg(test)]
    fn free_functions(&self) -> &[InstRef] {
        &self.free_functions
    }

    #[cfg(test)]
    fn free_functions_in_file(&self, file_id: FileId, name: Spur) -> &[InstRef] {
        self.free_functions_by_file_name
            .get(&(file_id, name))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn named_methods(&self) -> &[InstRef] {
        &self.named_methods
    }

    #[cfg(test)]
    fn anonymous_methods(&self) -> &[InstRef] {
        &self.anonymous_methods
    }

    /// Named destructor declarations in exact RIR order.
    ///
    /// These records remain private to one semantic epoch: their arena and
    /// interner handles are not durable semantic identities.
    pub(super) fn destructors(&self) -> &[RirDestructorDeclaration] {
        &self.destructors
    }
}

#[cfg(test)]
mod tests {
    use lasso::ThreadedRodeo;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::{AstGen, InstData};

    use super::*;

    fn lower_files(files: &[(&str, FileId)]) -> (Rir, ThreadedRodeo) {
        let mut interner = ThreadedRodeo::default();
        let mut items = Vec::new();
        for &(source, file_id) in files {
            let lexer = Lexer::with_interner_and_file_id(source, interner, file_id);
            let (tokens, next_interner) = lexer.tokenize().unwrap();
            let parser = Parser::new(tokens, next_interner);
            let (ast, next_interner) = parser.parse().unwrap();
            items.extend(ast.items);
            interner = next_interner;
        }
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&items);
        let rir = astgen.finish();
        (rir, interner)
    }

    fn names(indexed: &[InstRef], rir: &Rir, interner: &ThreadedRodeo) -> Vec<String> {
        indexed
            .iter()
            .map(|&inst_ref| {
                let InstData::FnDecl { name, .. } = &rir.get(inst_ref).data else {
                    panic!("function index contains a non-FnDecl instruction");
                };
                interner.resolve(name).to_owned()
            })
            .collect()
    }

    fn assert_rir_order(refs: &[InstRef]) {
        assert!(
            refs.windows(2)
                .all(|pair| pair[0].as_u32() < pair[1].as_u32()),
            "declarations are not in RIR order: {refs:?}"
        );
    }

    #[test]
    fn classifies_callable_owners_and_unclassified_constants_in_one_scan() {
        let source = r#"
            struct Named {
                value: i32,
                fn receiver(self) -> i32 { self.value }
                fn collide() -> i32 { 10 }
            }
            fn Factory() -> type {
                struct {
                    fn anon_receiver(self) -> i32 { 20 }
                    fn collide() -> i32 { 30 }
                }
            }
            fn generic(comptime T: type, value: T) -> T { value }
            fn collide() -> i32 { 40 }
            const alias = collide;
            drop fn Named(self) {}
            fn main() -> i32 { collide() }
        "#;
        let (rir, interner) = lower_files(&[(source, FileId::new(7))]);
        let index = RirDeclarationIndex::new(&rir);

        assert_eq!(
            names(index.free_functions(), &rir, &interner),
            ["Factory", "generic", "collide", "main"]
        );
        assert_eq!(
            names(index.named_methods(), &rir, &interner),
            ["receiver", "collide"]
        );
        assert_eq!(
            names(index.anonymous_methods(), &rir, &interner),
            ["anon_receiver", "collide"]
        );
        assert_rir_order(index.free_functions());
        assert_rir_order(index.named_methods());
        assert_rir_order(index.anonymous_methods());
        let collide = interner.get("collide").unwrap();
        assert!(
            index
                .first_free_function(collide, Some(FileId::new(7)))
                .is_some()
        );

        let destructors = index.destructors();
        assert_eq!(destructors.len(), 1);
        assert_eq!(interner.resolve(&destructors[0].type_name), "Named");
        assert_eq!(destructors[0].span.file_id, FileId::new(7));
        assert!(matches!(
            rir.get(destructors[0].declaration).data,
            InstData::DropFnDecl { body, .. } if body == destructors[0].body
        ));

        let expected_work = RirDeclarationIndexWork {
            build_invocations: 1,
            rir_instructions_visited: rir.len(),
            method_references_visited: 4,
            free_functions_indexed: 4,
            named_methods_indexed: 2,
            anonymous_methods_indexed: 2,
            destructors_indexed: 1,
            const_candidates_indexed: 1,
        };
        assert_eq!(index.work(), expected_work);
    }

    #[test]
    fn file_qualified_lookups_retain_duplicates_and_follow_rir_order() {
        let first = FileId::new(9);
        let second = FileId::new(2);
        let (rir, interner) = lower_files(&[
            (
                "fn same() -> i32 { 1 } fn same() -> i32 { 2 } fn main() -> i32 { same() }",
                first,
            ),
            ("fn same() -> i32 { 3 } const same = 4;", second),
        ]);
        let index = RirDeclarationIndex::new(&rir);
        let same = interner.get("same").unwrap();

        let first_candidates = index.free_functions_in_file(first, same);
        let second_candidates = index.free_functions_in_file(second, same);
        assert_eq!(first_candidates.len(), 2);
        assert_eq!(second_candidates.len(), 1);
        assert_rir_order(first_candidates);
        assert_rir_order(index.free_functions());
        assert_eq!(
            index
                .free_functions()
                .iter()
                .map(|&inst_ref| rir.get(inst_ref).span.file_id)
                .collect::<Vec<_>>(),
            [first, first, first, second]
        );
        assert_eq!(
            index.first_free_function(same, None),
            first_candidates.first().copied()
        );
        assert_eq!(
            index.first_free_function(same, Some(first)),
            first_candidates.first().copied()
        );
        assert_eq!(
            index.first_free_function(same, Some(second)),
            second_candidates.first().copied()
        );
        let rebuilt = RirDeclarationIndex::new(&rir);
        assert_eq!(rebuilt.work(), index.work());
        assert_eq!(rebuilt.free_functions(), index.free_functions());
    }

    #[test]
    fn same_named_destructors_across_files_follow_exact_rir_order() {
        let first = FileId::new(19);
        let second = FileId::new(3);
        let (rir, interner) = lower_files(&[
            ("struct Same { value: i32 } drop fn Same(self) {}", first),
            ("struct Same { value: bool } drop fn Same(self) {}", second),
        ]);
        let index = RirDeclarationIndex::new(&rir);
        let destructors = index.destructors();

        assert_eq!(destructors.len(), 2);
        assert_eq!(
            destructors
                .iter()
                .map(|record| record.span.file_id)
                .collect::<Vec<_>>(),
            [first, second]
        );
        assert!(
            destructors
                .windows(2)
                .all(|pair| { pair[0].declaration.as_u32() < pair[1].declaration.as_u32() })
        );
        assert!(
            destructors
                .iter()
                .all(|record| interner.resolve(&record.type_name) == "Same")
        );
        for record in destructors {
            assert!(matches!(
                rir.get(record.declaration).data,
                InstData::DropFnDecl { body, .. } if body == record.body
            ));
        }
    }
}
