//! Shared in-memory fixture for driving the production provider body path.
//!
//! The fixture drives [`analyze_provider_ordinary_body`] — the exact entry
//! point the compiler's body transaction uses — through an in-memory durable
//! fact source, so body analysis is exercised against the production
//! `ProviderBodyHost`/`OrdinaryBodyEngine` seam. A structural guard in
//! `provider_fixture_tests.rs` keeps this module off the retired
//! whole-program drivers by name.
//!
//! The fixture mirrors the production topology exactly:
//!
//! - The analyzed body's RIR bundle contains only that body's declaration,
//!   like the compiler's per-body plan materialization. Every other fact —
//!   callee signatures, nominal shapes, constants, members — crosses the
//!   provider boundary as explicit in-memory durable data.
//! - The durable source implements the same five `Durable*Source` contracts
//!   the compiler-side `CompilerBodyDurableSource` implements, with the same
//!   fail-closed shape: a fact that was not seeded resolves to `None`, and
//!   body analysis surfaces the miss as an ordinary spanned diagnostic.
//! - The `BodyFactProvider` value is a guard stub. The ordinary body path
//!   consults the durable source for every fact, so every stub operation
//!   panics; if body analysis ever starts consulting the provider object on
//!   this path, fixture tests fail loudly instead of silently absorbing it.

use ahash::AHashMap;
use std::rc::Rc;
use std::sync::Arc;

use rue_error::{CompileResult, PreviewFeatures};
use rue_lexer::Lexer;
use rue_parser::Parser;
use rue_rir::{AstGen, RirEditor, RirValidationContext, ValidatedRir};
use rue_span::FileId;
use rue_target::Target;

use super::provider::{
    DropCopyMetadata, ImportResolution, MemberCandidate, NameResolution, NominalWellFormedness,
    OperatorMemberCandidate, OperatorName, ProviderNamespace,
};
use super::{
    BodyFactProvider, BodyRirBundle, DurableAnonymousShape, DurableAnonymousSource,
    DurableBodyLookupSource, DurableCallableSource, DurableConst, DurableConstSource,
    DurableFunction, DurableMethod, DurableNominal, DurableNominalBody, DurableNominalSource,
    DurableSignatureParameter, ProviderOrdinaryBody, ProviderWellKnownOptionFacts,
    analyze_provider_ordinary_body,
};
use crate::types::LangItem;
use crate::{
    AnonymousNominalKey, SemanticImportConstValue, SemanticImportType, SemanticParameterMode,
    StableDefinitionKind, stable_digest,
};

/// The one durable definition key vocabulary of the fixture: a name, an
/// optional owner-type name, and the definition kind — the same identity parts
/// the compiler's `StableDefinitionKey` carries for a single module.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FixtureKey {
    pub(crate) name: Arc<str>,
    pub(crate) owner: Option<Arc<str>>,
    pub(crate) kind: StableDefinitionKind,
}

impl FixtureKey {
    pub(crate) fn function(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            owner: None,
            kind: StableDefinitionKind::Function,
        }
    }

    pub(crate) fn nominal(name: &str, kind: StableDefinitionKind) -> Self {
        Self {
            name: Arc::from(name),
            owner: None,
            kind,
        }
    }

    pub(crate) fn member(owner: &str, name: &str, kind: StableDefinitionKind) -> Self {
        Self {
            name: Arc::from(name),
            owner: Some(Arc::from(owner)),
            kind,
        }
    }

    pub(crate) fn destructor(owner: &str) -> Self {
        Self {
            name: Arc::from(owner),
            owner: Some(Arc::from(owner)),
            kind: StableDefinitionKind::Destructor,
        }
    }

    pub(crate) fn value_const(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            owner: None,
            kind: StableDefinitionKind::ValueConst,
        }
    }
}

/// The fixture's module identity: one logical module path.
pub(crate) type FixtureModule = Arc<str>;

pub(crate) type FixtureType = SemanticImportType<FixtureKey, FixtureModule>;
pub(crate) type FixtureConstValue = SemanticImportConstValue<FixtureKey, FixtureModule>;
pub(crate) type FixtureBody = ProviderOrdinaryBody<FixtureKey, FixtureModule>;

/// Explicit in-memory declaration facts for one single-module program.
#[derive(Clone, Default)]
pub(crate) struct FixtureFacts {
    module_path: Arc<str>,
    file: FileId,
    functions: AHashMap<FixtureKey, DurableFunction<FixtureKey, FixtureModule>>,
    methods: AHashMap<FixtureKey, DurableMethod<FixtureKey, FixtureModule>>,
    nominals: AHashMap<FixtureKey, DurableNominal<FixtureKey, FixtureModule>>,
    consts: AHashMap<FixtureKey, DurableConst<FixtureKey, FixtureModule>>,
}

/// The in-memory durable fact source handed to the production body host. Facts
/// that were never seeded answer `None`, matching the provider contract that a
/// miss is authoritative — body analysis then reports the miss as an ordinary
/// diagnostic instead of inventing a fact.
#[derive(Clone)]
pub(crate) struct FixtureFactSource(Rc<FixtureFacts>);

impl DurableNominalSource<FixtureKey, FixtureModule> for FixtureFactSource {
    fn nominal(&self, key: &FixtureKey) -> Option<DurableNominal<FixtureKey, FixtureModule>> {
        self.0.nominals.get(key).cloned()
    }

    fn nominal_file_id(&self, key: &FixtureKey) -> Option<FileId> {
        self.0.nominals.contains_key(key).then_some(self.0.file)
    }
}

impl DurableAnonymousSource<FixtureKey, FixtureModule> for FixtureFactSource {
    fn anonymous_shape(
        &self,
        _key: &AnonymousNominalKey<FixtureKey, FixtureModule>,
    ) -> Option<DurableAnonymousShape<FixtureKey, FixtureModule>> {
        // The fixture programs produce anonymous nominals inside the analyzed
        // body; none are imported from another body's durable facts.
        None
    }

    fn definition_symbol_component(&self, key: &FixtureKey) -> String {
        stable_digest::stable_definition_component(
            &self.0.module_path,
            &key.name,
            key.owner.as_deref(),
            key.kind as u8,
        )
    }

    fn module_symbol_component(&self, module: &FixtureModule) -> String {
        stable_digest::stable_module_component(module)
    }
}

impl DurableCallableSource<FixtureKey, FixtureModule> for FixtureFactSource {
    fn function(&self, key: &FixtureKey) -> Option<DurableFunction<FixtureKey, FixtureModule>> {
        self.0.functions.get(key).cloned()
    }

    fn method(&self, key: &FixtureKey) -> Option<DurableMethod<FixtureKey, FixtureModule>> {
        self.0.methods.get(key).cloned()
    }

    fn uses_deferred_body_type_placeholders(&self) -> bool {
        // Match the production source: comptime-typed signature slots are
        // deferred placeholders, not concrete `type` values.
        true
    }
}

impl DurableConstSource<FixtureKey, FixtureModule> for FixtureFactSource {
    fn constant(&self, key: &FixtureKey) -> Option<DurableConst<FixtureKey, FixtureModule>> {
        self.0.consts.get(key).cloned()
    }

    fn function_name(&self, key: &FixtureKey) -> Option<Arc<str>> {
        (key.kind == StableDefinitionKind::Function).then(|| key.name.clone())
    }
}

impl DurableBodyLookupSource<FixtureKey, FixtureModule> for FixtureFactSource {
    fn free_function(&self, _current: &FixtureKey, name: &str) -> Option<FixtureKey> {
        let key = FixtureKey::function(name);
        self.0.functions.contains_key(&key).then_some(key)
    }

    fn value_const(&self, _current: &FixtureKey, name: &str) -> Option<FixtureKey> {
        let key = FixtureKey::value_const(name);
        self.0.consts.contains_key(&key).then_some(key)
    }

    fn nominal(
        &self,
        _current: &FixtureKey,
        name: &str,
    ) -> Option<(FixtureKey, StableDefinitionKind)> {
        [StableDefinitionKind::Struct, StableDefinitionKind::Enum]
            .into_iter()
            .find_map(|kind| {
                let key = FixtureKey::nominal(name, kind);
                self.0.nominals.contains_key(&key).then_some((key, kind))
            })
    }

    fn named_member(
        &self,
        _current: &FixtureKey,
        owner: &str,
        name: &str,
    ) -> Option<(FixtureKey, bool)> {
        [
            StableDefinitionKind::Method,
            StableDefinitionKind::AssociatedFunction,
        ]
        .into_iter()
        .find_map(|kind| {
            let key = FixtureKey::member(owner, name, kind);
            let method = self.0.methods.get(&key)?;
            Some((key, method.has_self))
        })
    }

    fn root_module_binding(
        &self,
        _current: &FixtureKey,
        _name: &str,
    ) -> Option<super::DurableBodyModuleBinding<FixtureKey, FixtureModule>> {
        // The fixture is a single module with no `@import` bindings.
        None
    }

    fn module_binding(
        &self,
        _module: &FixtureModule,
        _name: &str,
    ) -> Option<super::DurableBodyModuleBinding<FixtureKey, FixtureModule>> {
        None
    }

    fn qualified_free_function(&self, _module: &FixtureModule, _name: &str) -> Option<FixtureKey> {
        None
    }

    fn qualified_value_const(&self, _module: &FixtureModule, _name: &str) -> Option<FixtureKey> {
        None
    }

    fn qualified_nominal(
        &self,
        _module: &FixtureModule,
        _name: &str,
    ) -> Option<(FixtureKey, StableDefinitionKind)> {
        None
    }

    fn module_path(&self, module: &FixtureModule) -> String {
        module.to_string()
    }

    fn definition_kind(&self, definition: &FixtureKey) -> Option<StableDefinitionKind> {
        Some(definition.kind)
    }

    fn definition_name(&self, definition: &FixtureKey) -> Option<Arc<str>> {
        Some(definition.name.clone())
    }

    fn definition_owner_name(&self, definition: &FixtureKey) -> Option<Arc<str>> {
        definition.owner.clone()
    }
}

/// Guard stub for the exact-fact provider boundary. The ordinary provider body
/// path answers every fact through the durable source, so no operation here is
/// reachable; a panic means body analysis grew a new provider consultation the
/// fixture (and the production wiring in `revisioned_query_database.rs`) must
/// learn about.
pub(crate) struct UnconsultedFactProvider;

macro_rules! unconsulted {
    () => {
        unreachable!("ordinary provider body analysis answers this fact through the durable source")
    };
}

impl BodyFactProvider for UnconsultedFactProvider {
    type ModuleRef = FixtureModule;
    type DeclarationRef = FixtureKey;
    type BodyInstanceRef = FixtureKey;
    type ReceiverType = Arc<str>;

    type DeclarationIdentity = ();
    type Signature = ();
    type ConstComptime = ();
    type ComptimeType = ();
    type ComptimeValue = ();
    type ComptimeCall = ();
    type AnonymousFacts = ();
    type ProducerBodyFacts = ();
    type ToolchainFacts = ();

    fn lookup_unqualified(
        &self,
        _module: &Self::ModuleRef,
        _namespace: ProviderNamespace,
        _name: &str,
    ) -> NameResolution {
        unconsulted!()
    }

    fn lookup_qualified(
        &self,
        _module: &Self::ModuleRef,
        _namespace: ProviderNamespace,
        _name: &str,
    ) -> NameResolution {
        unconsulted!()
    }

    fn method_candidates(
        &self,
        _receiver: &Self::ReceiverType,
        _name: &str,
    ) -> Vec<MemberCandidate<Self::DeclarationRef>> {
        unconsulted!()
    }

    fn operator_candidates(
        &self,
        _receiver: &Self::ReceiverType,
        _operator: OperatorName,
    ) -> Vec<OperatorMemberCandidate<Self::DeclarationRef>> {
        unconsulted!()
    }

    fn declaration_identity(
        &self,
        _decl: &Self::DeclarationRef,
    ) -> Option<Self::DeclarationIdentity> {
        unconsulted!()
    }

    fn signature(&self, _decl: &Self::DeclarationRef) -> Option<Self::Signature> {
        unconsulted!()
    }

    fn const_comptime(&self, _decl: &Self::DeclarationRef) -> Option<Self::ConstComptime> {
        unconsulted!()
    }

    fn reduce_comptime_call(
        &self,
        _decl: &Self::DeclarationRef,
        _type_arguments: &[(Arc<str>, Self::ComptimeType)],
        _value_arguments: &[(Arc<str>, Self::ComptimeValue)],
    ) -> Option<Self::ComptimeCall> {
        unconsulted!()
    }

    fn nominal_well_formedness(
        &self,
        _decl: &Self::DeclarationRef,
    ) -> Option<NominalWellFormedness> {
        unconsulted!()
    }

    fn anonymous_facts(&self, _decl: &Self::DeclarationRef) -> Option<Self::AnonymousFacts> {
        unconsulted!()
    }

    fn language_item(
        &self,
        _module: &Self::ModuleRef,
        _namespace: ProviderNamespace,
        _name: &str,
    ) -> Option<LangItem> {
        unconsulted!()
    }

    fn drop_copy_metadata(&self, _receiver: &Self::ReceiverType) -> Option<DropCopyMetadata> {
        unconsulted!()
    }

    fn resolve_import(&self, _module: &Self::ModuleRef, _specifier: &str) -> ImportResolution {
        unconsulted!()
    }

    fn producer_body_facts(
        &self,
        _instance: &Self::BodyInstanceRef,
    ) -> Option<Self::ProducerBodyFacts> {
        unconsulted!()
    }

    fn trusted_toolchain_facts(&self, _instance: &Self::BodyInstanceRef) -> Self::ToolchainFacts {
        unconsulted!()
    }
}

pub(crate) fn value_param(
    name: &str,
    ty: FixtureType,
) -> DurableSignatureParameter<FixtureKey, FixtureModule> {
    DurableSignatureParameter {
        name: Arc::from(name),
        ty,
        mode: SemanticParameterMode::Value,
        is_comptime: false,
    }
}

pub(crate) fn mode_param(
    name: &str,
    ty: FixtureType,
    mode: SemanticParameterMode,
) -> DurableSignatureParameter<FixtureKey, FixtureModule> {
    DurableSignatureParameter {
        name: Arc::from(name),
        ty,
        mode,
        is_comptime: false,
    }
}

/// Declaration-shape knobs for a fixture method beyond the value-receiver
/// default: receiver mode, `self`-lessness (associated functions), and the
/// accessor flag — the same fields the durable method fact carries.
pub(crate) struct MethodShape {
    pub(crate) has_self: bool,
    pub(crate) self_mode: SemanticParameterMode,
    pub(crate) is_accessor: bool,
}

impl Default for MethodShape {
    fn default() -> Self {
        Self {
            has_self: true,
            self_mode: SemanticParameterMode::Value,
            is_accessor: false,
        }
    }
}

/// Structural knobs for a fixture struct beyond the copyable default: linear
/// marking, `__drop` presence, and a language-item binding — the same fields
/// the durable nominal fact carries.
#[derive(Default)]
pub(crate) struct StructShape {
    pub(crate) is_linear: bool,
    pub(crate) has_destructor: bool,
    pub(crate) lang_item: Option<LangItem>,
    pub(crate) is_repr_c: bool,
}

/// Builder for one single-module provider fixture: explicit durable
/// declaration facts plus the production analysis helper.
pub(crate) struct ProviderFixture {
    facts: FixtureFacts,
    preview: PreviewFeatures,
    well_known: ProviderWellKnownOptionFacts<FixtureKey, FixtureModule>,
}

impl ProviderFixture {
    pub(crate) fn new() -> Self {
        Self {
            facts: FixtureFacts {
                module_path: Arc::from("fixture/main.rue"),
                file: FileId::DEFAULT,
                ..FixtureFacts::default()
            },
            preview: PreviewFeatures::new(),
            well_known: ProviderWellKnownOptionFacts {
                nominals: Vec::new(),
                option_by_payload: Vec::new(),
            },
        }
    }

    pub(crate) fn with_preview(preview: PreviewFeatures) -> Self {
        let mut fixture = Self::new();
        fixture.preview = preview;
        fixture
    }

    pub(crate) fn declare_function(
        &mut self,
        name: &str,
        parameters: Vec<DurableSignatureParameter<FixtureKey, FixtureModule>>,
        result: FixtureType,
    ) -> FixtureKey {
        let key = FixtureKey::function(name);
        self.facts.functions.insert(
            key.clone(),
            DurableFunction {
                parameters: parameters.into(),
                result,
                type_syntax: None,
                is_public: true,
                is_unchecked: false,
                is_extern: false,
            },
        );
        key
    }

    pub(crate) fn declare_struct(
        &mut self,
        name: &str,
        fields: Vec<(&str, FixtureType)>,
        is_copy: bool,
    ) -> FixtureKey {
        self.declare_struct_with(name, fields, is_copy, StructShape::default())
    }

    pub(crate) fn declare_struct_with(
        &mut self,
        name: &str,
        fields: Vec<(&str, FixtureType)>,
        is_copy: bool,
        shape: StructShape,
    ) -> FixtureKey {
        let key = FixtureKey::nominal(name, StableDefinitionKind::Struct);
        self.facts.nominals.insert(
            key.clone(),
            DurableNominal {
                name: Arc::from(name),
                module_path: self.facts.module_path.clone(),
                is_public: true,
                is_builtin: false,
                lang_item: shape.lang_item,
                is_repr_c: shape.is_repr_c,
                has_destructor: shape.has_destructor,
                body: DurableNominalBody::Struct {
                    fields: fields
                        .into_iter()
                        .map(|(field, ty)| (Arc::from(field), ty))
                        .collect(),
                    is_copy,
                    is_linear: shape.is_linear,
                },
            },
        );
        key
    }

    pub(crate) fn declare_enum(
        &mut self,
        name: &str,
        variants: Vec<(&str, Vec<FixtureType>)>,
    ) -> FixtureKey {
        let key = FixtureKey::nominal(name, StableDefinitionKind::Enum);
        self.facts.nominals.insert(
            key.clone(),
            DurableNominal {
                name: Arc::from(name),
                module_path: self.facts.module_path.clone(),
                is_public: true,
                is_builtin: false,
                lang_item: None,
                is_repr_c: false,
                has_destructor: false,
                body: DurableNominalBody::Enum {
                    variants: variants
                        .into_iter()
                        .map(|(variant, payloads)| (Arc::from(variant), payloads.into()))
                        .collect(),
                },
            },
        );
        key
    }

    pub(crate) fn declare_method(
        &mut self,
        owner: &FixtureKey,
        name: &str,
        parameters: Vec<DurableSignatureParameter<FixtureKey, FixtureModule>>,
        result: FixtureType,
    ) -> FixtureKey {
        self.declare_method_with(owner, name, parameters, result, MethodShape::default())
    }

    pub(crate) fn declare_method_with(
        &mut self,
        owner: &FixtureKey,
        name: &str,
        parameters: Vec<DurableSignatureParameter<FixtureKey, FixtureModule>>,
        result: FixtureType,
        shape: MethodShape,
    ) -> FixtureKey {
        let kind = if shape.has_self {
            StableDefinitionKind::Method
        } else {
            StableDefinitionKind::AssociatedFunction
        };
        let key = FixtureKey::member(&owner.name, name, kind);
        self.facts.methods.insert(
            key.clone(),
            DurableMethod {
                receiver: SemanticImportType::Nominal(owner.clone()),
                parameters: parameters.into(),
                result,
                type_syntax: None,
                has_self: shape.has_self,
                self_mode: shape.self_mode,
                is_accessor: shape.is_accessor,
            },
        );
        key
    }

    pub(crate) fn declare_const(
        &mut self,
        name: &str,
        ty: FixtureType,
        value: FixtureConstValue,
    ) -> FixtureKey {
        let key = FixtureKey::value_const(name);
        self.facts.consts.insert(
            key.clone(),
            DurableConst {
                is_public: true,
                ty,
                value,
            },
        );
        key
    }

    /// Run the production provider body path over the free function `function`
    /// declared in `source`, which must contain exactly that declaration —
    /// mirroring the compiler's per-body plan, whose RIR bundle carries one
    /// declaration and nothing else. Every contextual fact must come from the
    /// seeded durable data, exactly as it does across the production provider
    /// boundary.
    pub(crate) fn analyze(&self, source: &str, function: &str) -> CompileResult<FixtureBody> {
        self.analyze_declaration(
            source,
            FixtureKey::function(function),
            function,
            StableDefinitionKind::Function,
            None,
            |_| {},
        )
    }

    /// Run the production provider body path over a named member body. The
    /// single declaration in `source` is the owning nominal, whose analyzed
    /// member is selected by `owner`/`name`/kind exactly as the compiler's
    /// member body transaction selects it.
    pub(crate) fn analyze_member(
        &self,
        source: &str,
        owner: &str,
        name: &str,
        kind: StableDefinitionKind,
    ) -> CompileResult<FixtureBody> {
        self.analyze_declaration(
            source,
            FixtureKey::member(owner, name, kind),
            name,
            kind,
            Some(owner),
            |_| {},
        )
    }

    /// Run the production provider body path over the destructor body of
    /// `owner`. The single declaration in `source` is the `drop fn` item.
    pub(crate) fn analyze_destructor(
        &self,
        source: &str,
        owner: &str,
    ) -> CompileResult<FixtureBody> {
        self.analyze_declaration(
            source,
            FixtureKey::destructor(owner),
            owner,
            StableDefinitionKind::Destructor,
            Some(owner),
            |_| {},
        )
    }

    /// [`Self::analyze`] with a RIR edit applied between AstGen lowering and
    /// validation, for probing analysis behavior on instruction shapes the
    /// frontend cannot produce (for example a malformed internal intrinsic).
    pub(crate) fn analyze_edited(
        &self,
        source: &str,
        function: &str,
        edit: impl FnOnce(&mut RirEditor),
    ) -> CompileResult<FixtureBody> {
        self.analyze_declaration(
            source,
            FixtureKey::function(function),
            function,
            StableDefinitionKind::Function,
            None,
            edit,
        )
    }

    fn analyze_declaration(
        &self,
        source: &str,
        key: FixtureKey,
        name: &str,
        kind: StableDefinitionKind,
        owner_name: Option<&str>,
        edit: impl FnOnce(&mut RirEditor),
    ) -> CompileResult<FixtureBody> {
        let (tokens, interner) = Lexer::new(source).tokenize().expect("fixture source lexes");
        let (ast, interner) = Parser::new(tokens, interner)
            .parse()
            .expect("fixture source parses");
        assert_eq!(
            ast.items.len(),
            1,
            "the analyzed body plan carries exactly one declaration; \
             seed further context as durable facts instead"
        );
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let mut editor = astgen.finish_editor();
        edit(&mut editor);
        let source_lengths = [(self.facts.file, source.len() as u32)];
        let rir = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &source_lengths,
            },
        )
        .expect("fixture RIR validates");
        let bundle = BodyRirBundle::new(rir, interner);
        let facts = FixtureFactSource(Rc::new(self.facts.clone()));
        analyze_provider_ordinary_body(
            &UnconsultedFactProvider,
            facts,
            &bundle,
            key,
            name,
            kind,
            owner_name,
            Target::host().expect("host target resolves"),
            self.preview.clone(),
            &self.well_known,
        )
    }
}

pub(crate) fn error_source_slice<'s>(source: &'s str, error: &rue_error::CompileError) -> &'s str {
    let span = error.span().expect("diagnostic carries its source span");
    &source[span.start as usize..span.end as usize]
}
