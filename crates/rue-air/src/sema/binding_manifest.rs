//! Owned semantic bindings emitted only after declaration binding succeeds.

use std::sync::{Arc, OnceLock};

use lasso::Spur;
use rue_error::MultiErrorResult;
use rue_rir::InstData;
use rue_span::{FileId, Span};

use super::RirDeclarationIndexWork;
use super::{Sema, SemaOutput};

/// Structural descriptors for one completed declaration-binding pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeclarationBindingWork {
    pub bind_invocations: usize,
    /// Size of the input RIR, not a claim that binding visited every entry.
    pub input_rir_instructions: usize,
    pub declaration_index_build_invocations: usize,
    pub indexed_free_functions: usize,
    pub indexed_named_methods: usize,
    pub indexed_anonymous_methods: usize,
    pub indexed_destructors: usize,
    pub indexed_const_candidates: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBindingNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SemanticBindingKind {
    Function,
    Struct,
    Enum,
    ValueConst,
    ModuleBinding,
    Destructor,
    Method,
    AssociatedFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBinding {
    /// Request-local source file containing this declaration.
    pub file_id: FileId,
    /// Request-local declaration location; excluded from stable identity.
    pub declaration_span: Span,
    pub namespace: SemanticBindingNamespace,
    pub kind: SemanticBindingKind,
    pub name: Arc<str>,
    pub owner: Option<Arc<str>>,
    /// Source visibility; destructors are never public.
    pub is_public: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SemanticBindingManifestWork {
    pub build_invocations: usize,
    pub rir_instructions_visited: usize,
    pub bindings_emitted: usize,
    pub functions_emitted: usize,
    pub types_emitted: usize,
    pub constants_emitted: usize,
    pub module_bindings_emitted: usize,
    pub destructors_emitted: usize,
    pub named_methods_emitted: usize,
    pub named_method_edges_visited: usize,
    pub anonymous_methods_deferred: usize,
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
    pub source_text_clones: usize,
}

#[derive(Debug, Clone)]
pub struct SemanticBindingManifest {
    bindings: Arc<[SemanticBinding]>,
    work: SemanticBindingManifestWork,
}

impl SemanticBindingManifest {
    /// Successful bindings in deterministic declaration order, with named
    /// methods grouped beneath their owning named struct.
    pub fn bindings(&self) -> &[SemanticBinding] {
        &self.bindings
    }
    pub fn work(&self) -> SemanticBindingManifestWork {
        self.work
    }
}

pub struct BoundSema<'a> {
    sema: Sema<'a>,
    manifest: OnceLock<SemanticBindingManifest>,
    binding_work: DeclarationBindingWork,
}

impl<'a> BoundSema<'a> {
    pub fn binding_work(&self) -> DeclarationBindingWork {
        self.binding_work
    }
    /// Materialize the owned manifest on demand. Ordinary body analysis does
    /// not pay for this additional RIR traversal.
    pub fn binding_manifest(&self) -> &SemanticBindingManifest {
        self.manifest
            .get_or_init(|| self.sema.build_binding_manifest())
    }

    /// Whether a caller has requested the optional binding manifest.
    pub fn manifest_is_materialized(&self) -> bool {
        self.manifest.get().is_some()
    }

    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        self.sema.analyze_all_bodies()
    }
}

impl<'a> Sema<'a> {
    pub(super) fn into_bound(self) -> BoundSema<'a> {
        let index = self.declaration_index.work();
        BoundSema {
            binding_work: DeclarationBindingWork::from_inputs(self.rir.len(), index),
            sema: self,
            manifest: OnceLock::new(),
        }
    }

    fn build_binding_manifest(&self) -> SemanticBindingManifest {
        let mut bindings = Vec::new();
        let mut work = SemanticBindingManifestWork {
            build_invocations: 1,
            anonymous_methods_deferred: self.declaration_index.work().anonymous_methods_indexed,
            ..SemanticBindingManifestWork::default()
        };
        for (inst_ref, inst) in self.rir.iter() {
            work.rir_instructions_visited += 1;
            let mut emit = |file_id: FileId,
                            declaration_span: Span,
                            namespace: SemanticBindingNamespace,
                            kind: SemanticBindingKind,
                            name: &Spur,
                            owner: Option<Arc<str>>,
                            is_public: bool| {
                assert_eq!(file_id, declaration_span.file_id);
                bindings.push(SemanticBinding {
                    file_id,
                    declaration_span,
                    namespace,
                    kind,
                    name: Arc::from(self.interner.resolve(name)),
                    owner,
                    is_public,
                })
            };
            match &inst.data {
                InstData::FnDecl { name, is_pub, .. }
                    if !self.declaration_index.is_type_scoped_method(inst_ref) =>
                {
                    assert!(
                        self.functions_by_file_name
                            .contains_key(&(inst.span.file_id, *name)),
                        "manifest free function must be a bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Value,
                        SemanticBindingKind::Function,
                        name,
                        None,
                        *is_pub,
                    );
                    work.functions_emitted += 1;
                }
                InstData::StructDecl {
                    name,
                    methods_start,
                    methods_len,
                    is_pub,
                    ..
                } => {
                    let struct_id = *self
                        .structs_by_file_name
                        .get(&(inst.span.file_id, *name))
                        .expect("manifest struct must be a bound winner");
                    let owner: Arc<str> = Arc::from(self.interner.resolve(name));
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Type,
                        SemanticBindingKind::Struct,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                    for method_ref in self.rir.get_inst_refs(*methods_start, *methods_len) {
                        work.named_method_edges_visited += 1;
                        let method_inst = self.rir.get(method_ref);
                        let InstData::FnDecl {
                            name,
                            has_self,
                            is_pub,
                            ..
                        } = &method_inst.data
                        else {
                            unreachable!("named struct method edge must target FnDecl");
                        };
                        assert_eq!(
                            self.named_method_declarations.get(&(struct_id, *name)),
                            Some(&method_ref),
                            "manifest named method must be the bound winner"
                        );
                        emit(
                            method_inst.span.file_id,
                            method_inst.span,
                            SemanticBindingNamespace::Method,
                            if *has_self {
                                SemanticBindingKind::Method
                            } else {
                                SemanticBindingKind::AssociatedFunction
                            },
                            name,
                            Some(owner.clone()),
                            *is_pub,
                        );
                        work.named_methods_emitted += 1;
                    }
                }
                InstData::EnumDecl { name, is_pub, .. } => {
                    assert!(
                        self.enums_by_file_name
                            .contains_key(&(inst.span.file_id, *name)),
                        "manifest enum must be a bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Type,
                        SemanticBindingKind::Enum,
                        name,
                        None,
                        *is_pub,
                    );
                    work.types_emitted += 1;
                }
                InstData::ConstDecl { name, is_pub, .. } => {
                    let key = (inst.span.file_id, *name);
                    let kind = if self.module_bindings.contains_key(&key) {
                        work.module_bindings_emitted += 1;
                        SemanticBindingKind::ModuleBinding
                    } else if self.constants_by_file_name.contains_key(&key) {
                        work.constants_emitted += 1;
                        SemanticBindingKind::ValueConst
                    } else {
                        panic!("manifest const must be a classified bound winner")
                    };
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Value,
                        kind,
                        name,
                        None,
                        *is_pub,
                    );
                }
                InstData::DropFnDecl { type_name, .. } => {
                    let struct_id = *self
                        .structs_by_file_name
                        .get(&(inst.span.file_id, *type_name))
                        .expect("manifest destructor target must be a bound named struct");
                    assert_eq!(
                        self.destructor_spans.get(&struct_id),
                        Some(&inst.span),
                        "manifest destructor must be the bound winner"
                    );
                    emit(
                        inst.span.file_id,
                        inst.span,
                        SemanticBindingNamespace::Destructor,
                        SemanticBindingKind::Destructor,
                        type_name,
                        Some(Arc::from(self.interner.resolve(type_name))),
                        false,
                    );
                    work.destructors_emitted += 1;
                }
                _ => {}
            }
        }
        work.bindings_emitted = bindings.len();
        SemanticBindingManifest {
            bindings: bindings.into(),
            work,
        }
    }
}

impl DeclarationBindingWork {
    fn from_inputs(input_rir_instructions: usize, index: RirDeclarationIndexWork) -> Self {
        Self {
            bind_invocations: 1,
            input_rir_instructions,
            declaration_index_build_invocations: index.build_invocations,
            indexed_free_functions: index.free_functions_indexed,
            indexed_named_methods: index.named_methods_indexed,
            indexed_anonymous_methods: index.anonymous_methods_indexed,
            indexed_destructors: index.destructors_indexed,
            indexed_const_candidates: index.const_candidates_indexed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rue_error::{CompileErrors, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::AstGen;

    use super::*;

    fn bind(source: &str) -> Result<SemanticBindingManifest, CompileErrors> {
        let (tokens, interner) = Lexer::new(source)
            .tokenize()
            .map_err(CompileErrors::from_error)?;
        let (ast, interner) = Parser::new(tokens, interner).parse()?;
        let rir = AstGen::new(&ast, &interner).generate();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new()).bind_declarations()?;
        Ok(bound.binding_manifest().clone())
    }

    fn bind_with_module_paths(source: &str) -> Result<SemanticBindingManifest, CompileErrors> {
        let (tokens, interner) = Lexer::new(source)
            .tokenize()
            .map_err(CompileErrors::from_error)?;
        let (ast, interner) = Parser::new(tokens, interner).parse()?;
        let rir = AstGen::new(&ast, &interner).generate();
        let mut sema = Sema::new(&rir, &interner, PreviewFeatures::new());
        sema.set_root_file_id(FileId::DEFAULT);
        sema.set_file_paths(HashMap::from([
            (FileId::DEFAULT, "/main.rue".to_owned()),
            (FileId::new(1), "/other.rue".to_owned()),
        ]));
        let bound = sema.bind_declarations()?;
        Ok(bound.binding_manifest().clone())
    }

    #[test]
    fn manifest_is_owned_deterministic_and_complete_after_binding() {
        let source = r#"
            struct Resource {
                value: i32,
                fn get(self) -> i32 { self.value }
                fn make() -> Resource { Resource { value: 0 } }
            }
            enum Choice { None, Some(i32) }
            const LIMIT: i32 = 4;
            drop fn Resource(self) {}
            fn helper() -> i32 { LIMIT }
            fn main() -> i32 { helper() }
        "#;
        let first = bind(source).unwrap();
        let second = bind(source).unwrap();
        assert_eq!(first.bindings(), second.bindings());
        assert_eq!(first.work(), second.work());
        assert_eq!(first.work().build_invocations, 1);
        assert_eq!(first.work().functions_emitted, 2);
        assert_eq!(first.work().types_emitted, 2);
        assert_eq!(first.work().constants_emitted, 1);
        assert_eq!(first.work().destructors_emitted, 1);
        assert_eq!(first.work().named_methods_emitted, 2);
        assert_eq!(first.work().named_method_edges_visited, 2);
        assert_eq!(
            first.work().named_method_edges_visited,
            first.work().named_methods_emitted
        );
        assert_eq!(first.work().anonymous_methods_deferred, 0);
        assert_eq!(first.work().bindings_emitted, 8);
        assert_eq!(first.work().parser_invocations, 0);
        assert_eq!(first.work().ast_payload_clones, 0);
        assert_eq!(first.work().source_text_clones, 0);
        assert!(first.bindings().iter().any(|binding| {
            binding.name.as_ref() == "make"
                && binding.owner.as_deref() == Some("Resource")
                && binding.kind == SemanticBindingKind::AssociatedFunction
        }));
        assert!(
            first
                .bindings()
                .iter()
                .all(|binding| binding.file_id == binding.declaration_span.file_id)
        );
    }

    #[test]
    fn rejected_duplicate_method_never_produces_a_manifest() {
        let error = bind("struct Bad { fn duplicate(self) {} fn duplicate(self) {} } fn main() {}")
            .unwrap_err();
        assert!(error.to_string().contains("duplicate"));
    }

    #[test]
    fn synthetic_public_method_visibility_is_preserved() {
        let (tokens, interner) =
            Lexer::new("struct PublicApi { fn exposed(self) {} } fn main() {}")
                .tokenize()
                .unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut rir = AstGen::new(&ast, &interner).generate();
        let method = rir
            .iter()
            .find_map(|(reference, inst)| match inst.data {
                InstData::FnDecl { has_self: true, .. } => Some(reference),
                _ => None,
            })
            .unwrap();
        let InstData::FnDecl { is_pub, .. } = &mut rir.get_mut(method).data else {
            unreachable!()
        };
        *is_pub = true;
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(bound.binding_manifest().bindings().iter().any(|binding| {
            binding.name.as_ref() == "exposed"
                && binding.kind == SemanticBindingKind::Method
                && binding.is_public
        }));
    }

    #[test]
    fn rejected_const_collision_and_duplicate_destructor_emit_no_manifest() {
        assert!(bind("const same: i32 = 1; fn same() {} fn main() {}").is_err());
        assert!(
            bind(
                "struct Resource {} drop fn Resource(self) {} drop fn Resource(self) {} fn main() {}"
            )
            .is_err()
        );
    }

    #[test]
    fn constants_are_classified_only_after_successful_evaluation() {
        let manifest = bind_with_module_paths(
            "const value: i32 = 1; const imported = @import(\"other.rue\"); fn main() {}",
        )
        .unwrap();
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "value" && binding.kind == SemanticBindingKind::ValueConst
        }));
        assert!(manifest.bindings().iter().any(|binding| {
            binding.name.as_ref() == "imported"
                && binding.kind == SemanticBindingKind::ModuleBinding
        }));
        assert_eq!(manifest.work().constants_emitted, 1);
        assert_eq!(manifest.work().module_bindings_emitted, 1);
    }

    #[test]
    fn analyze_all_matches_explicit_bind_then_analyze() {
        let source = "fn helper(x: i32) -> i32 { x + 1 } fn main() -> i32 { helper(41) }";
        let (tokens, interner) = Lexer::new(source).tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();
        let direct = Sema::new(&rir, &interner, PreviewFeatures::new())
            .analyze_all()
            .unwrap();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        let explicit = bound.analyze_all_bodies().unwrap();
        let summarize = |output: &SemaOutput| {
            (
                output
                    .functions
                    .iter()
                    .map(|function| {
                        (
                            function.name.clone(),
                            function.air.display_with_interner(&interner).to_string(),
                        )
                    })
                    .collect::<Vec<_>>(),
                output.strings.clone(),
                output
                    .warnings
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                output.type_pool.stats(),
                output.body_analysis_work,
            )
        };
        assert_eq!(summarize(&direct), summarize(&explicit));
    }

    #[test]
    fn manifest_scan_is_lazy_and_materialized_only_on_request() {
        let (tokens, interner) = Lexer::new("fn main() {}").tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let rir = AstGen::new(&ast, &interner).generate();
        let bound = Sema::new(&rir, &interner, PreviewFeatures::new())
            .bind_declarations()
            .unwrap();
        assert!(!bound.manifest_is_materialized());
        assert_eq!(bound.binding_work().bind_invocations, 1);
        assert_eq!(bound.binding_work().input_rir_instructions, rir.len());
        assert_eq!(bound.binding_work().declaration_index_build_invocations, 1);
        assert_eq!(bound.binding_manifest().work().build_invocations, 1);
        assert!(bound.manifest_is_materialized());
    }

    #[test]
    fn anonymous_methods_are_explicitly_deferred() {
        let manifest = bind(
            "fn Factory(comptime T: type) -> type { struct { value: T, fn get(self) -> T { self.value } } } fn main() {}",
        )
        .unwrap();
        assert_eq!(manifest.work().anonymous_methods_deferred, 1);
        assert!(
            !manifest
                .bindings()
                .iter()
                .any(|binding| binding.name.as_ref() == "get")
        );
    }
}
