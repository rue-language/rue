//! Stable source-definition identities issued only after semantic binding.

use std::collections::HashMap;
use std::sync::Arc;

use rue_air::{
    DeclarationBindingWork, Sema, SemanticBinding, SemanticBindingKind,
    SemanticBindingManifestWork, SemanticBindingNamespace,
};
use rue_error::{CompileError, CompileErrors, ErrorKind, MultiErrorResult};
use rue_parser::ast::Visibility;
use rue_span::Span;
use rue_target::Target;

use crate::{
    CanonicalMergedProgram, CanonicalRirOutput, DefinitionKind, DefinitionNamespace,
    DefinitionOccurrenceId, ModuleId, PreviewFeatures, SourceRevision,
};

#[derive(Debug)]
struct DefinitionIssuer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionNamespace {
    Value,
    Type,
    Destructor,
    Method,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StableDefinitionKind {
    Function,
    Struct,
    Enum,
    ValueConst,
    ModuleBinding,
    Destructor,
    Method,
    AssociatedFunction,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableNamedTypeKey {
    module: ModuleId,
    kind: StableDefinitionKind,
    name: Arc<str>,
}

impl StableNamedTypeKey {
    pub fn module(&self) -> &ModuleId {
        &self.module
    }
    pub fn kind(&self) -> StableDefinitionKind {
        self.kind
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableDefinitionKey {
    module: ModuleId,
    namespace: StableDefinitionNamespace,
    kind: StableDefinitionKind,
    name: Arc<str>,
    owner: Option<StableNamedTypeKey>,
}

impl StableDefinitionKey {
    pub fn module(&self) -> &ModuleId {
        &self.module
    }
    pub fn namespace(&self) -> StableDefinitionNamespace {
        self.namespace
    }
    pub fn kind(&self) -> StableDefinitionKind {
        self.kind
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn owner(&self) -> Option<&StableNamedTypeKey> {
        self.owner.as_ref()
    }
}

#[derive(Debug, Clone)]
pub struct BoundDefinitionId {
    key: StableDefinitionKey,
    issuer: Arc<DefinitionIssuer>,
}

impl BoundDefinitionId {
    pub fn stable_key(&self) -> &StableDefinitionKey {
        &self.key
    }
}

impl PartialEq for BoundDefinitionId {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && Arc::ptr_eq(&self.issuer, &other.issuer)
    }
}
impl Eq for BoundDefinitionId {}

#[derive(Debug, Clone)]
pub struct BoundDefinitionRecord {
    id: BoundDefinitionId,
    occurrence: Option<DefinitionOccurrenceId>,
    declaration_span: Span,
    visibility: Option<Visibility>,
}

impl BoundDefinitionRecord {
    pub fn id(&self) -> &BoundDefinitionId {
        &self.id
    }
    pub fn stable_key(&self) -> &StableDefinitionKey {
        self.id.stable_key()
    }
    pub fn occurrence(&self) -> Option<DefinitionOccurrenceId> {
        self.occurrence
    }
    pub fn declaration_span(&self) -> Span {
        self.declaration_span
    }
    pub fn visibility(&self) -> Option<Visibility> {
        self.visibility
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoundDefinitionWork {
    pub modules_validated: usize,
    pub manifest_bindings_visited: usize,
    pub top_level_occurrences_joined: usize,
    pub named_methods_issued: usize,
    pub anonymous_methods_deferred: usize,
    pub ids_issued: usize,
    pub parser_invocations: usize,
    pub ast_payload_clones: usize,
    pub source_text_clones: usize,
}

#[derive(Debug, Clone)]
pub struct BoundDefinitionSet {
    source_revision: SourceRevision,
    issuer: Arc<DefinitionIssuer>,
    definitions: Arc<[BoundDefinitionRecord]>,
    manifest_work: SemanticBindingManifestWork,
    work: BoundDefinitionWork,
}

impl BoundDefinitionSet {
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    pub fn definitions(&self) -> &[BoundDefinitionRecord] {
        &self.definitions
    }
    pub fn manifest_work(&self) -> SemanticBindingManifestWork {
        self.manifest_work
    }
    pub fn work(&self) -> BoundDefinitionWork {
        self.work
    }

    pub fn definition<'a>(
        &'a self,
        id: &BoundDefinitionId,
        revision: &SourceRevision,
    ) -> Result<&'a BoundDefinitionRecord, CompileError> {
        if revision != &self.source_revision {
            return Err(invalid(
                "bound definition lookup used a foreign source revision",
            ));
        }
        if !Arc::ptr_eq(&self.issuer, &id.issuer) {
            return Err(invalid("bound definition ID belongs to a foreign issuer"));
        }
        self.definitions
            .binary_search_by(|record| record.stable_key().cmp(&id.key))
            .ok()
            .map(|index| &self.definitions[index])
            .ok_or_else(|| invalid("bound definition ID is absent from its issuing set"))
    }
}

pub fn bind_canonical_definitions(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    preview_features: PreviewFeatures,
    target: Target,
) -> MultiErrorResult<BoundDefinitionSet> {
    bind_canonical_definitions_with_work(merged, rir, preview_features, target)
        .map(|(definitions, _)| definitions)
}

pub(crate) fn bind_canonical_definitions_with_work(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    preview_features: PreviewFeatures,
    target: Target,
) -> MultiErrorResult<(BoundDefinitionSet, DeclarationBindingWork)> {
    let sema = configure_canonical_sema(merged, rir, preview_features, target)?;
    let bound = sema.bind_declarations()?;
    let binding_work = bound.binding_work();
    let manifest = bound.binding_manifest();
    let definitions = issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    )
    .map_err(CompileErrors::from)?;
    Ok((definitions, binding_work))
}

pub(crate) fn configure_canonical_sema<'a>(
    merged: &CanonicalMergedProgram,
    rir: &'a CanonicalRirOutput,
    preview_features: PreviewFeatures,
    target: Target,
) -> MultiErrorResult<Sema<'a>> {
    if merged.ast().source_revision() != rir.source_revision() {
        return Err(CompileErrors::from(invalid(
            "canonical syntax and RIR have different source revisions",
        )));
    }
    if !rir
        .semantic_symbols()
        .admits_exact_modules(merged.ast().modules())
    {
        return Err(CompileErrors::from(invalid(
            "canonical RIR belongs to foreign parsed module artifacts",
        )));
    }

    let mut sema = Sema::new_for_target(
        rir.rir(),
        rir.semantic_symbols().interner(),
        preview_features,
        target,
    );
    let root = merged
        .ast()
        .modules()
        .iter()
        .find(|module| module.module_id() == merged.ast().root())
        .expect("canonical root module is admitted");
    sema.set_root_file_id(root.file_id());
    sema.set_file_paths(
        merged
            .ast()
            .modules()
            .iter()
            .map(|module| (module.file_id(), module.physical_path().to_owned()))
            .collect(),
    );
    sema.set_symbol_paths(
        merged
            .ast()
            .modules()
            .iter()
            .map(|module| (module.file_id(), module.module_id().as_str().to_owned()))
            .collect(),
    );
    Ok(sema)
}

pub(crate) fn issue_bound_definitions(
    merged: &CanonicalMergedProgram,
    revision: &SourceRevision,
    bindings: &[SemanticBinding],
    manifest_work: SemanticBindingManifestWork,
) -> Result<BoundDefinitionSet, CompileError> {
    let modules = merged.ast().modules();
    let by_file = modules
        .iter()
        .map(|module| (module.file_id(), module))
        .collect::<HashMap<_, _>>();
    let issuer = Arc::new(DefinitionIssuer);
    let mut records = Vec::with_capacity(bindings.len());
    let mut work = BoundDefinitionWork {
        modules_validated: modules.len(),
        manifest_bindings_visited: bindings.len(),
        anonymous_methods_deferred: manifest_work.anonymous_methods_deferred,
        ..BoundDefinitionWork::default()
    };
    for binding in bindings {
        validate_binding_shape(binding)?;
        let module = by_file.get(&binding.file_id).ok_or_else(|| {
            invalid("semantic binding references a file absent from canonical syntax")
        })?;
        if binding.declaration_span.file_id != binding.file_id {
            return Err(invalid("semantic binding span has a mismatched file ID"));
        }
        let stable_kind = stable_kind(binding.kind);
        let owner = binding.owner.as_ref().map(|owner| StableNamedTypeKey {
            module: module.module_id().clone(),
            kind: StableDefinitionKind::Struct,
            name: owner.clone(),
        });
        let key = StableDefinitionKey {
            module: module.module_id().clone(),
            namespace: stable_namespace(binding.namespace),
            kind: stable_kind,
            name: binding.name.clone(),
            owner,
        };
        let (occurrence, visibility) = if binding.namespace == SemanticBindingNamespace::Method {
            work.named_methods_issued += 1;
            (
                None,
                Some(if binding.is_public {
                    Visibility::Public
                } else {
                    Visibility::Private
                }),
            )
        } else {
            let syntax_kind = syntax_kind(binding.kind);
            let namespace = if binding.kind == SemanticBindingKind::Destructor {
                DefinitionNamespace::Destructor
            } else {
                DefinitionNamespace::ModuleItem
            };
            let name_key = crate::DefinitionNameKey::new(
                module.module_id().clone(),
                namespace,
                binding.name.as_ref(),
            );
            let matches = merged
                .definitions()
                .definitions_named(&name_key)
                .filter(|record| record.kind() == syntax_kind)
                .collect::<Vec<_>>();
            let [winner] = matches.as_slice() else {
                return Err(invalid(
                    "semantic winner does not join exactly one syntax occurrence",
                ));
            };
            if winner.declaration_span() != binding.declaration_span {
                return Err(invalid(
                    "semantic winner span does not match its syntax occurrence",
                ));
            }
            work.top_level_occurrences_joined += 1;
            let expected_public = winner.visibility() == Some(Visibility::Public);
            if binding.kind != SemanticBindingKind::Destructor
                && binding.is_public != expected_public
            {
                return Err(invalid(
                    "semantic winner visibility does not match its syntax occurrence",
                ));
            }
            (Some(winner.id()), winner.visibility())
        };
        records.push(BoundDefinitionRecord {
            id: BoundDefinitionId {
                key,
                issuer: issuer.clone(),
            },
            occurrence,
            declaration_span: binding.declaration_span,
            visibility,
        });
    }
    records.sort_by(|left, right| left.stable_key().cmp(right.stable_key()));
    if records
        .windows(2)
        .any(|pair| pair[0].stable_key() == pair[1].stable_key())
    {
        return Err(invalid(
            "semantic binding produced duplicate stable definition keys",
        ));
    }
    for record in &records {
        if let Some(owner) = record.stable_key().owner() {
            let owner_key = StableDefinitionKey {
                module: owner.module.clone(),
                namespace: StableDefinitionNamespace::Type,
                kind: owner.kind,
                name: owner.name.clone(),
                owner: None,
            };
            if records
                .binary_search_by(|candidate| candidate.stable_key().cmp(&owner_key))
                .is_err()
            {
                return Err(invalid(
                    "member or destructor owner has no bound named-type definition",
                ));
            }
        }
    }
    work.ids_issued = records.len();
    Ok(BoundDefinitionSet {
        source_revision: revision.clone(),
        issuer,
        definitions: records.into(),
        manifest_work,
        work,
    })
}

fn validate_binding_shape(binding: &SemanticBinding) -> Result<(), CompileError> {
    let valid = match (binding.namespace, binding.kind, binding.owner.as_deref()) {
        (SemanticBindingNamespace::Value, SemanticBindingKind::Function, None)
        | (SemanticBindingNamespace::Value, SemanticBindingKind::ValueConst, None)
        | (SemanticBindingNamespace::Value, SemanticBindingKind::ModuleBinding, None)
        | (SemanticBindingNamespace::Type, SemanticBindingKind::Struct, None)
        | (SemanticBindingNamespace::Type, SemanticBindingKind::Enum, None) => true,
        (
            SemanticBindingNamespace::Method,
            SemanticBindingKind::Method | SemanticBindingKind::AssociatedFunction,
            Some(_),
        ) => true,
        (SemanticBindingNamespace::Destructor, SemanticBindingKind::Destructor, Some(owner)) => {
            owner == binding.name.as_ref() && !binding.is_public
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(invalid(
            "semantic binding has an impossible namespace/kind/owner shape",
        ))
    }
}

fn stable_namespace(value: SemanticBindingNamespace) -> StableDefinitionNamespace {
    match value {
        SemanticBindingNamespace::Value => StableDefinitionNamespace::Value,
        SemanticBindingNamespace::Type => StableDefinitionNamespace::Type,
        SemanticBindingNamespace::Destructor => StableDefinitionNamespace::Destructor,
        SemanticBindingNamespace::Method => StableDefinitionNamespace::Method,
    }
}

fn stable_kind(value: SemanticBindingKind) -> StableDefinitionKind {
    match value {
        SemanticBindingKind::Function => StableDefinitionKind::Function,
        SemanticBindingKind::Struct => StableDefinitionKind::Struct,
        SemanticBindingKind::Enum => StableDefinitionKind::Enum,
        SemanticBindingKind::ValueConst => StableDefinitionKind::ValueConst,
        SemanticBindingKind::ModuleBinding => StableDefinitionKind::ModuleBinding,
        SemanticBindingKind::Destructor => StableDefinitionKind::Destructor,
        SemanticBindingKind::Method => StableDefinitionKind::Method,
        SemanticBindingKind::AssociatedFunction => StableDefinitionKind::AssociatedFunction,
    }
}

fn syntax_kind(value: SemanticBindingKind) -> DefinitionKind {
    match value {
        SemanticBindingKind::Function => DefinitionKind::Function,
        SemanticBindingKind::Struct => DefinitionKind::Struct,
        SemanticBindingKind::Enum => DefinitionKind::Enum,
        SemanticBindingKind::ValueConst | SemanticBindingKind::ModuleBinding => {
            DefinitionKind::Const
        }
        SemanticBindingKind::Destructor => DefinitionKind::Destructor,
        SemanticBindingKind::Method | SemanticBindingKind::AssociatedFunction => {
            unreachable!("methods have no top-level syntax occurrence")
        }
    }
}

fn invalid(message: impl Into<String>) -> CompileError {
    CompileError::without_span(ErrorKind::InvalidCompilerInput(message.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rue_span::FileId;

    use super::*;
    use crate::parsed_modules::parse_source_snapshot_modules;
    use crate::{SourceMetadata, SourceSnapshot, lower_canonical_rir, merge_parsed_modules};

    fn snapshot(entries: &[(u32, &str, &str, &str)], root: u32) -> SourceSnapshot {
        let physical = entries
            .iter()
            .map(|(id, path, _, _)| (FileId::new(*id), (*path).to_owned()))
            .collect::<HashMap<_, _>>();
        let logical = entries
            .iter()
            .map(|(id, _, logical, _)| (FileId::new(*id), (*logical).to_owned()))
            .collect::<HashMap<_, _>>();
        let metadata = SourceMetadata::new(FileId::new(root), physical, logical).unwrap();
        SourceSnapshot::new(
            metadata,
            entries
                .iter()
                .map(|(id, _, _, text)| (FileId::new(*id), Arc::new((*text).to_owned())))
                .collect(),
        )
        .unwrap()
    }

    fn bind(snapshot: &SourceSnapshot) -> BoundDefinitionSet {
        let parsed = parse_source_snapshot_modules(snapshot).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        bind_canonical_definitions(&merged, &rir, PreviewFeatures::new(), Target::default())
            .unwrap()
    }

    fn keys(set: &BoundDefinitionSet) -> Vec<StableDefinitionKey> {
        set.definitions()
            .iter()
            .map(|record| record.stable_key().clone())
            .collect()
    }

    const PROGRAM: &str = r#"
        struct Resource {
            value: i32,
            fn get(self) -> i32 { self.value }
            fn make() -> Resource { Resource { value: 0 } }
        }
        enum Choice { None, Some(i32) }
        const LIMIT: i32 = 4;
        drop fn Resource(self) {}
        fn main() -> i32 { Resource.make().get() + LIMIT }
    "#;

    #[test]
    fn stable_keys_ignore_relocation_file_ids_and_batch_order() {
        let first = snapshot(
            &[
                (9, "/old/z.rue", "z.rue", "fn helper() -> i32 { 1 }"),
                (2, "/old/main.rue", "main.rue", PROGRAM),
            ],
            2,
        );
        let second = snapshot(
            &[
                (71, "/new/main.rue", "main.rue", PROGRAM),
                (4, "/new/z.rue", "z.rue", "fn helper() -> i32 { 1 }"),
            ],
            71,
        );
        let first = bind(&first);
        let second = bind(&second);
        assert_eq!(first.source_revision(), second.source_revision());
        assert_eq!(keys(&first), keys(&second));
        assert_eq!(first.work(), second.work());
    }

    #[test]
    fn rename_and_module_move_change_only_the_stable_identity_components() {
        let original = bind(&snapshot(&[(1, "/a.rue", "a.rue", "fn main() {}")], 1));
        let renamed = bind(&snapshot(&[(7, "/a.rue", "a.rue", "fn renamed() {}")], 7));
        let moved = bind(&snapshot(&[(8, "/b.rue", "b.rue", "fn main() {}")], 8));
        assert_ne!(keys(&original), keys(&renamed));
        assert_ne!(keys(&original), keys(&moved));
        assert_eq!(original.definitions()[0].stable_key().name(), "main");
        assert_eq!(
            moved.definitions()[0].stable_key().module().as_str(),
            "b.rue"
        );
    }

    #[test]
    fn issuer_and_revision_provenance_fail_closed() {
        let source = snapshot(&[(1, "/a.rue", "a.rue", "fn main() {}")], 1);
        let first = bind(&source);
        let second = bind(&source);
        let first_id = first.definitions()[0].id().clone();
        assert_eq!(first_id, first_id.clone());
        assert_ne!(first_id, second.definitions()[0].id().clone());
        assert!(
            second
                .definition(&first_id, second.source_revision())
                .is_err()
        );

        let foreign = bind(&snapshot(&[(2, "/a.rue", "a.rue", "fn renamed() {}")], 2));
        assert!(
            first
                .definition(&first_id, foreign.source_revision())
                .is_err()
        );
        assert!(first.definition(&first_id, first.source_revision()).is_ok());
    }

    #[test]
    fn occurrences_methods_owners_and_metrics_are_retained_separately() {
        let set = bind(&snapshot(&[(1, "/main.rue", "main.rue", PROGRAM)], 1));
        let method = set
            .definitions()
            .iter()
            .find(|record| record.stable_key().name() == "get")
            .unwrap();
        assert_eq!(method.stable_key().kind(), StableDefinitionKind::Method);
        assert_eq!(method.stable_key().owner().unwrap().name(), "Resource");
        assert!(method.occurrence().is_none());
        assert_eq!(method.visibility(), Some(Visibility::Private));
        assert!(
            set.definitions()
                .iter()
                .filter(
                    |record| record.stable_key().namespace() != StableDefinitionNamespace::Method
                )
                .all(|record| record.occurrence().is_some())
        );
        assert_eq!(set.work().ids_issued, set.definitions().len());
        assert_eq!(set.work().named_methods_issued, 2);
        assert_eq!(set.work().anonymous_methods_deferred, 0);
        assert_eq!(set.work().parser_invocations, 0);
        assert_eq!(set.work().ast_payload_clones, 0);
        assert_eq!(set.work().source_text_clones, 0);
    }

    #[test]
    fn semantic_rejection_and_foreign_canonical_artifacts_issue_no_ids() {
        let collision = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const same: i32 = 1; fn same() {} fn main() {}",
            )],
            1,
        );
        let parsed = parse_source_snapshot_modules(&collision).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        assert!(
            bind_canonical_definitions(&merged, &rir, PreviewFeatures::new(), Target::default())
                .is_err()
        );

        let source = snapshot(&[(2, "/main.rue", "main.rue", "fn main() {}")], 2);
        let first = parse_source_snapshot_modules(&source).unwrap();
        let second = parse_source_snapshot_modules(&source).unwrap();
        let first = merge_parsed_modules(&first).unwrap();
        let second = merge_parsed_modules(&second).unwrap();
        let foreign_rir = lower_canonical_rir(&second).unwrap();
        assert!(
            bind_canonical_definitions(
                &first,
                &foreign_rir,
                PreviewFeatures::new(),
                Target::default()
            )
            .is_err()
        );
    }

    #[test]
    fn bound_definition_set_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoundDefinitionSet>();
    }
}
