//! Stable source-definition identities issued only after semantic binding.

use std::collections::HashMap;
use std::sync::Arc;

use rue_air::{
    DeclarationBindingWork, Sema, SemanticBinding, SemanticBindingKind,
    SemanticBindingManifestWork, SemanticBindingNamespace,
};
use rue_error::{CompileError, CompileErrors, ErrorKind, MultiErrorResult};
use rue_parser::ast::{Item, Visibility};
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

    #[cfg(test)]
    pub(crate) fn for_test(
        module: ModuleId,
        namespace: StableDefinitionNamespace,
        kind: StableDefinitionKind,
        name: impl Into<Arc<str>>,
        owner: Option<(StableDefinitionKind, Arc<str>)>,
    ) -> Self {
        let owner = owner.map(|(kind, name)| StableNamedTypeKey {
            module: module.clone(),
            kind,
            name,
        });
        Self {
            module,
            namespace,
            kind,
            name: name.into(),
            owner,
        }
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
    input_partition: BoundDefinitionInputPartition,
}

/// Parser-authored boundaries for hashing a declaration without treating its
/// executable payload as part of its signature.
#[derive(Debug, Clone)]
pub(crate) enum BoundDefinitionInputPartition {
    Body { signature: Span, body: Span },
    Initializer { signature: Span, initializer: Span },
    ExactSignature(Arc<[Span]>),
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
    pub(crate) fn input_partition(&self) -> BoundDefinitionInputPartition {
        self.input_partition.clone()
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

    /// Look up a definition by its stable, issuer-independent source key.
    pub fn definition_by_key(&self, key: &StableDefinitionKey) -> Option<&BoundDefinitionRecord> {
        self.definitions
            .binary_search_by(|record| record.stable_key().cmp(key))
            .ok()
            .map(|index| &self.definitions[index])
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
        self.definition_by_key(&id.key)
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

/// Bind once and export stable, request-independent declaration semantics
/// before the successful binder is consumed. This performs no body analysis,
/// second bind, syntax reconstruction, or additional RIR traversal.
pub fn bind_canonical_declaration_semantics(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    preview_features: PreviewFeatures,
    target: Target,
) -> MultiErrorResult<(
    BoundDefinitionSet,
    Arc<[crate::DurableDeclarationSemantic]>,
    rue_air::SemanticDeclarationExportWork,
)> {
    let sema = configure_canonical_sema(merged, rir, preview_features, target)?;
    let bound = sema.bind_declarations()?;
    let manifest = bound.binding_manifest();
    let definitions = issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    )
    .map_err(CompileErrors::from)?;
    let converted = bound
        .with_declaration_semantics(|records, work| {
            (
                crate::durable_semantics::convert_declaration_semantics(
                    merged,
                    &definitions,
                    records,
                ),
                work,
            )
        })
        .map_err(|failure| {
            CompileErrors::from(invalid(&format!(
                "durable semantic AIR export failed: {failure:?}"
            )))
        })?;
    let semantics = converted.0.map_err(|failure| {
        CompileErrors::from(invalid(&format!(
            "durable semantic conversion failed: {failure:?}"
        )))
    })?;
    Ok((definitions, semantics, converted.1))
}

/// Comparison-only proof seam for the durable projection/installation path.
///
/// This deliberately does not cache or skip production work. It resolves an
/// authoritative ordinary epoch, projects its durable result into fresh
/// current-revision shells, installs atomically, and proves that re-exporting
/// the installed epoch produces the same stable semantics before exercising
/// body analysis.
pub fn compare_canonical_durable_declaration_install(
    merged: &CanonicalMergedProgram,
    rir: &CanonicalRirOutput,
    preview_features: PreviewFeatures,
    target: Target,
) -> MultiErrorResult<(crate::DurableSemanticProjectionWork, DeclarationBindingWork)> {
    let ordinary = configure_canonical_sema(merged, rir, preview_features.clone(), target)?
        .bind_declarations()?;
    let manifest = ordinary.binding_manifest();
    let definitions = issue_bound_definitions(
        merged,
        rir.source_revision(),
        manifest.bindings(),
        manifest.work(),
    )
    .map_err(CompileErrors::from)?;
    let durable = ordinary
        .with_declaration_semantics(|records, _| {
            crate::durable_semantics::convert_declaration_semantics(merged, &definitions, records)
        })
        .map_err(|failure| {
            CompileErrors::from(invalid(format!(
                "ordinary semantic AIR export failed: {failure:?}"
            )))
        })?
        .map_err(|failure| {
            CompileErrors::from(invalid(format!(
                "ordinary durable semantic conversion failed: {failure:?}"
            )))
        })?;
    let shells = configure_canonical_sema(merged, rir, preview_features, target)?
        .predeclare_declaration_shells()?;
    let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
    let (projected, projection_work) = crate::project_durable_declaration_semantics(
        merged,
        &definitions,
        &shell_records,
        &durable,
    )
    .map_err(|failure| {
        CompileErrors::from(invalid(format!(
            "durable semantic projection failed: {failure:?}"
        )))
    })?;
    let installed = shells
        .install_declaration_semantics(&projected)
        .map_err(|failure| {
            CompileErrors::from(invalid(format!(
                "durable semantic installation failed: {failure:?}"
            )))
        })?;
    let binding_work = installed.binding_work();
    let installed_durable = installed
        .with_declaration_semantics(|records, _| {
            crate::durable_semantics::convert_declaration_semantics(merged, &definitions, records)
        })
        .map_err(|failure| {
            CompileErrors::from(invalid(format!(
                "installed semantic AIR export failed: {failure:?}"
            )))
        })?
        .map_err(|failure| {
            CompileErrors::from(invalid(format!(
                "installed durable semantic conversion failed: {failure:?}"
            )))
        })?;
    if installed_durable != durable {
        return Err(CompileErrors::from(invalid(
            "projected durable install changed declaration semantics",
        )));
    }
    let ordinary_bodies = ordinary.analyze_all_bodies();
    let installed_bodies = installed.analyze_all_bodies();
    match (ordinary_bodies, installed_bodies) {
        (Ok(ordinary), Ok(installed)) => {
            let ordinary = crate::build_functions_and_cfgs(
                ordinary,
                crate::OptLevel::default(),
                rir.semantic_symbols().interner(),
            )?;
            let installed = crate::build_functions_and_cfgs(
                installed,
                crate::OptLevel::default(),
                rir.semantic_symbols().interner(),
            )?;
            if format!("{:?}", ordinary.functions) != format!("{:?}", installed.functions)
                || format!("{:?}", ordinary.warnings) != format!("{:?}", installed.warnings)
                || ordinary.strings != installed.strings
            {
                return Err(CompileErrors::from(invalid(
                    "projected durable install changed body, CFG, or diagnostic artifacts",
                )));
            }
        }
        (Err(ordinary), Err(installed)) if format!("{ordinary:?}") == format!("{installed:?}") => {}
        _ => {
            return Err(CompileErrors::from(invalid(
                "projected durable install changed body-analysis diagnostics",
            )));
        }
    }
    Ok((projection_work, binding_work))
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
        let input_partition = definition_input_partition(module, binding)?;
        records.push(BoundDefinitionRecord {
            id: BoundDefinitionId {
                key,
                issuer: issuer.clone(),
            },
            occurrence,
            declaration_span: binding.declaration_span,
            visibility,
            input_partition,
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

fn definition_input_partition(
    module: &crate::parsed_modules::ParsedModule,
    binding: &SemanticBinding,
) -> Result<BoundDefinitionInputPartition, CompileError> {
    let ast = module.ast();
    let signature_and_body = |declaration: Span, body: Span| {
        partition_prefix(declaration, body)
            .map(|signature| BoundDefinitionInputPartition::Body { signature, body })
    };
    let signature_and_initializer = |declaration: Span, initializer: Span| {
        partition_prefix(declaration, initializer).map(|signature| {
            BoundDefinitionInputPartition::Initializer {
                signature,
                initializer,
            }
        })
    };

    if binding.namespace == SemanticBindingNamespace::Method {
        binding
            .owner
            .as_deref()
            .ok_or_else(|| invalid("named method binding has no owner"))?;
        let method = ast.items.iter().find_map(|item| match item {
            Item::Struct(structure) => structure
                .methods
                .iter()
                .find(|method| method.span == binding.declaration_span),
            _ => None,
        });
        return method
            .ok_or_else(|| invalid("named method binding does not join canonical syntax"))
            .and_then(|method| signature_and_body(method.span, method.body.span()));
    }

    let item = ast.items.iter().find(|item| match item {
        Item::Function(value) => value.span == binding.declaration_span,
        Item::Struct(value) => value.span == binding.declaration_span,
        Item::Enum(value) => value.span == binding.declaration_span,
        Item::DropFn(value) => value.span == binding.declaration_span,
        Item::Const(value) => value.span == binding.declaration_span,
        Item::Error(_) => false,
    });
    match item {
        Some(Item::Function(value)) => signature_and_body(value.span, value.body.span()),
        Some(Item::DropFn(value)) => signature_and_body(value.span, value.body.span()),
        Some(Item::Const(value)) => signature_and_initializer(value.span, value.init.span()),
        Some(Item::Struct(value)) => Ok(BoundDefinitionInputPartition::ExactSignature(
            signature_fragments_excluding_method_bodies(value)?.into(),
        )),
        Some(Item::Enum(value)) => Ok(BoundDefinitionInputPartition::ExactSignature(
            vec![value.span].into(),
        )),
        Some(Item::Error(_)) | None => Err(invalid(
            "semantic binding does not join an authoritative canonical syntax item",
        )),
    }
}

fn signature_fragments_excluding_method_bodies(
    structure: &rue_parser::ast::StructDecl,
) -> Result<Vec<Span>, CompileError> {
    let mut fragments = Vec::with_capacity(structure.methods.len() + 1);
    let mut cursor = structure.span.start;
    for method in &structure.methods {
        let body = method.body.span();
        if body.file_id != structure.span.file_id
            || body.start < cursor
            || body.end > structure.span.end
            || body.start >= body.end
        {
            return Err(invalid(
                "struct method body span is not ordered within its declaration",
            ));
        }
        fragments.push(Span::with_file(structure.span.file_id, cursor, body.start));
        cursor = body.end;
    }
    fragments.push(Span::with_file(
        structure.span.file_id,
        cursor,
        structure.span.end,
    ));
    Ok(fragments)
}

fn partition_prefix(declaration: Span, payload: Span) -> Result<Span, CompileError> {
    if declaration.file_id != payload.file_id
        || payload.start < declaration.start
        || payload.end > declaration.end
        || payload.start >= payload.end
    {
        return Err(invalid(
            "semantic input payload span is not contained by its declaration",
        ));
    }
    Ok(Span::with_file(
        declaration.file_id,
        declaration.start,
        payload.start,
    ))
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

    fn export(
        snapshot: &SourceSnapshot,
    ) -> (
        BoundDefinitionSet,
        Arc<[crate::DurableDeclarationSemantic]>,
        rue_air::SemanticDeclarationExportWork,
    ) {
        let parsed = parse_source_snapshot_modules(snapshot).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        bind_canonical_declaration_semantics(
            &merged,
            &rir,
            PreviewFeatures::new(),
            Target::default(),
        )
        .unwrap()
    }

    fn keys(set: &BoundDefinitionSet) -> Vec<StableDefinitionKey> {
        set.definitions()
            .iter()
            .map(|record| record.stable_key().clone())
            .collect()
    }

    fn compare(
        snapshot: &SourceSnapshot,
    ) -> (crate::DurableSemanticProjectionWork, DeclarationBindingWork) {
        let parsed = parse_source_snapshot_modules(snapshot).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        compare_canonical_durable_declaration_install(
            &merged,
            &rir,
            PreviewFeatures::new(),
            Target::default(),
        )
        .unwrap()
    }

    #[test]
    fn projected_install_matches_ordinary_across_relocation_order_modules_methods_and_drop() {
        let root = r#"
            struct Resource {
                value: i32,
                fn get(self) -> i32 { self.value }
                fn make(value: i32) -> Resource { Resource { value } }
            }
            enum Choice { None, Some(Resource) }
            drop fn Resource(self) {}
            fn main() -> i32 { Resource.make(1).get() }
        "#;
        let sibling = "struct Sibling { value: bool } fn helper(value: i32) -> i32 { value }";
        let first = snapshot(
            &[
                (9, "/old/sibling.rue", "sibling.rue", sibling),
                (2, "/old/main.rue", "main.rue", root),
            ],
            2,
        );
        let relocated = snapshot(
            &[
                (71, "/new/main.rue", "main.rue", root),
                (4, "/new/sibling.rue", "sibling.rue", sibling),
            ],
            71,
        );
        for input in [&first, &relocated] {
            let (projection, install) = compare(input);
            assert_eq!(projection.projection_invocations, 1);
            assert_eq!(projection.rir_instructions_visited, 0);
            assert_eq!(
                projection.definition_records_indexed,
                projection.shells_visited
            );
            assert_eq!(
                projection.definition_lookup_probes,
                projection.shells_visited
            );
            assert_eq!(
                projection.shells_visited,
                projection.durable_records_visited
            );
            assert_eq!(install.declaration_resolution_invocations, 0);
            assert_eq!(install.durable_install_invocations, 1);
            assert_eq!(
                install.durable_payloads_installed,
                projection.shells_visited
            );
        }

        let (_, old_durable, _) = export(&first);
        let parsed = parse_source_snapshot_modules(&relocated).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let current_definitions = bind(&relocated);
        let shells =
            configure_canonical_sema(&merged, &rir, PreviewFeatures::new(), Target::default())
                .unwrap()
                .predeclare_declaration_shells()
                .unwrap();
        let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
        let (projected, work) = crate::project_durable_declaration_semantics(
            &merged,
            &current_definitions,
            &shell_records,
            &old_durable,
        )
        .unwrap();
        assert_eq!(projected.len(), old_durable.len());
        assert_eq!(work.definition_records_indexed, old_durable.len());
        assert_eq!(work.definition_lookup_probes, old_durable.len());
    }

    #[test]
    fn projection_definition_join_work_is_linear_for_128_modules() {
        let owned = (0..128_u32)
            .map(|index| {
                let id = index + 1;
                let physical = format!("/src/module_{index}.rue");
                let logical = format!("module_{index}.rue");
                let source = if index == 0 {
                    "fn main() -> i32 { 0 }".to_owned()
                } else {
                    format!("fn f{index}() -> i32 {{ {index} }}")
                };
                (id, physical, logical, source)
            })
            .collect::<Vec<_>>();
        let borrowed = owned
            .iter()
            .map(|(id, physical, logical, source)| {
                (*id, physical.as_str(), logical.as_str(), source.as_str())
            })
            .collect::<Vec<_>>();
        let input = snapshot(&borrowed, 1);
        let (definitions, durable, _) = export(&input);
        let parsed = parse_source_snapshot_modules(&input).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let shells =
            configure_canonical_sema(&merged, &rir, PreviewFeatures::new(), Target::default())
                .unwrap()
                .predeclare_declaration_shells()
                .unwrap();
        let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
        let (projected, projection) = crate::project_durable_declaration_semantics(
            &merged,
            &definitions,
            &shell_records,
            &durable,
        )
        .unwrap();

        assert_eq!(projection.shells_visited, 128);
        assert_eq!(projection.durable_records_visited, 128);
        assert_eq!(projection.definition_records_indexed, 128);
        assert_eq!(projection.definition_lookup_probes, 128);
        assert_eq!(projection.rir_instructions_visited, 0);
        assert_eq!(projected.len(), 128);
    }

    #[test]
    fn duplicate_projection_input_fails_before_shells_are_consumed() {
        let input = snapshot(&[(1, "/main.rue", "main.rue", "fn main() -> i32 { 0 }")], 1);
        let (definitions, durable, _) = export(&input);
        let parsed = parse_source_snapshot_modules(&input).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let shells =
            configure_canonical_sema(&merged, &rir, PreviewFeatures::new(), Target::default())
                .unwrap()
                .predeclare_declaration_shells()
                .unwrap();
        let shell_records = shells.declaration_shells().cloned().collect::<Vec<_>>();
        let duplicated = durable
            .iter()
            .cloned()
            .chain(durable.iter().cloned())
            .collect::<Vec<_>>();

        assert_eq!(
            crate::project_durable_declaration_semantics(
                &merged,
                &definitions,
                &shell_records,
                &duplicated,
            )
            .unwrap_err(),
            crate::DurableSemanticProjectionFailure::DuplicateDefinition
        );
        let (projected, _) = crate::project_durable_declaration_semantics(
            &merged,
            &definitions,
            &shell_records,
            &durable,
        )
        .unwrap();
        let installed = shells.install_declaration_semantics(&projected).unwrap();
        assert_eq!(installed.binding_work().durable_payloads_installed, 1);
    }

    #[test]
    fn unsupported_const_projection_fails_without_partial_install() {
        let input = snapshot(
            &[(
                1,
                "/main.rue",
                "main.rue",
                "const X: i32 = 1; fn main() -> i32 { X }",
            )],
            1,
        );
        let parsed = parse_source_snapshot_modules(&input).unwrap();
        let merged = merge_parsed_modules(&parsed).unwrap();
        let rir = lower_canonical_rir(&merged).unwrap();
        let error = compare_canonical_durable_declaration_install(
            &merged,
            &rir,
            PreviewFeatures::new(),
            Target::default(),
        )
        .unwrap_err();
        assert!(
            format!("{error:?}").contains("UnsupportedDeclaration"),
            "{error:?}"
        );
        // The failed candidate was consumed; a fresh ordinary epoch remains valid.
        configure_canonical_sema(&merged, &rir, PreviewFeatures::new(), Target::default())
            .unwrap()
            .bind_declarations()
            .unwrap()
            .analyze_all_bodies()
            .unwrap();
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
    fn durable_declaration_export_is_relocation_and_order_stable_without_extra_rir() {
        let first = snapshot(
            &[
                (
                    9,
                    "/old/z.rue",
                    "z.rue",
                    "fn helper(x: ptr const i32) -> bool { true } const alias = helper;",
                ),
                (2, "/old/main.rue", "main.rue", PROGRAM),
            ],
            2,
        );
        let moved = snapshot(
            &[
                (71, "/new/main.rue", "main.rue", PROGRAM),
                (
                    4,
                    "/new/z.rue",
                    "z.rue",
                    "fn helper(x: ptr const i32) -> bool { true } const alias = helper;",
                ),
            ],
            71,
        );
        let (_, first, first_work) = export(&first);
        let (_, moved, moved_work) = export(&moved);
        assert_eq!(first, moved);
        assert_eq!(first_work, moved_work);
        assert_eq!(first_work.build_invocations, 1);
        assert_eq!(first_work.rir_instructions_visited, 0);
        assert!(first.iter().any(|record| matches!(
            record.payload,
            crate::DurableDeclarationPayload::Const {
                value: crate::DurableConstValue::Function(_),
                ..
            }
        )));
    }

    #[test]
    fn durable_member_export_joins_same_named_members_through_their_stable_owner() {
        const MEMBERS: &str = r#"
            struct Alpha {
                fn shared(self) -> i32 { 1 }
                fn make() -> i32 { 2 }
            }
            struct Beta {
                fn shared(self) -> bool { true }
                fn make() -> bool { false }
            }
            fn main() {}
        "#;
        let first = snapshot(
            &[
                (9, "/old/z.rue", "z.rue", "fn helper() {}"),
                (2, "/old/main.rue", "main.rue", MEMBERS),
            ],
            2,
        );
        let moved = snapshot(
            &[
                (71, "/new/main.rue", "main.rue", MEMBERS),
                (4, "/new/z.rue", "z.rue", "fn helper() {}"),
            ],
            71,
        );
        let (_, first, _) = export(&first);
        let (_, moved, _) = export(&moved);
        assert_eq!(first, moved);

        let members = first
            .iter()
            .filter(|record| {
                matches!(
                    record.key.kind(),
                    StableDefinitionKind::Method | StableDefinitionKind::AssociatedFunction
                )
            })
            .map(|record| {
                (
                    record.key.kind(),
                    record.key.name().to_owned(),
                    record.key.owner().unwrap().name().to_owned(),
                    record.payload.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 4);
        for name in ["make", "shared"] {
            let same_name = members
                .iter()
                .filter(|member| member.1 == name)
                .collect::<Vec<_>>();
            assert_eq!(same_name.len(), 2);
            assert_ne!(same_name[0].2, same_name[1].2);
            assert_ne!(same_name[0].3, same_name[1].3);
        }
    }

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

    #[test]
    fn authoritative_payload_partition_rejects_foreign_reversed_and_empty_spans() {
        let declaration = Span::with_file(FileId::new(1), 10, 30);
        assert!(partition_prefix(declaration, Span::with_file(FileId::new(2), 20, 25)).is_err());
        assert!(partition_prefix(declaration, Span::with_file(FileId::new(1), 25, 20)).is_err());
        assert!(partition_prefix(declaration, Span::with_file(FileId::new(1), 20, 20)).is_err());
        assert!(partition_prefix(declaration, Span::with_file(FileId::new(1), 9, 20)).is_err());
        assert!(partition_prefix(declaration, Span::with_file(FileId::new(1), 20, 31)).is_err());
    }
}
