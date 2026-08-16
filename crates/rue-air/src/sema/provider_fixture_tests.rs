//! Direct tests for the production provider body path.
//!
//! These tests drive [`analyze_provider_ordinary_body`] — the exact entry point
//! the compiler's body transaction uses — through an in-memory durable fact
//! source, so body analysis is exercised against the production
//! `ProviderBodyHost`/`OrdinaryBodyEngine` seam rather than the retired
//! whole-program `Sema` test drivers in `tests.rs`. A structural guard at the
//! bottom of this file keeps the fixture off those drivers by name.
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
//!   this path, these tests fail loudly instead of silently absorbing it.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use rue_error::{CompileResult, ErrorKind, PreviewFeatures};
use rue_lexer::Lexer;
use rue_parser::Parser;
use rue_rir::{AstGen, RirValidationContext, ValidatedRir};
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
struct FixtureKey {
    name: Arc<str>,
    owner: Option<Arc<str>>,
    kind: StableDefinitionKind,
}

impl FixtureKey {
    fn function(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            owner: None,
            kind: StableDefinitionKind::Function,
        }
    }

    fn nominal(name: &str, kind: StableDefinitionKind) -> Self {
        Self {
            name: Arc::from(name),
            owner: None,
            kind,
        }
    }

    fn member(owner: &str, name: &str, kind: StableDefinitionKind) -> Self {
        Self {
            name: Arc::from(name),
            owner: Some(Arc::from(owner)),
            kind,
        }
    }

    fn value_const(name: &str) -> Self {
        Self {
            name: Arc::from(name),
            owner: None,
            kind: StableDefinitionKind::ValueConst,
        }
    }
}

/// The fixture's module identity: one logical module path.
type FixtureModule = Arc<str>;

type FixtureType = SemanticImportType<FixtureKey, FixtureModule>;
type FixtureConstValue = SemanticImportConstValue<FixtureKey, FixtureModule>;

/// Explicit in-memory declaration facts for one single-module program.
#[derive(Clone, Default)]
struct FixtureFacts {
    module_path: Arc<str>,
    file: FileId,
    functions: HashMap<FixtureKey, DurableFunction<FixtureKey, FixtureModule>>,
    methods: HashMap<FixtureKey, DurableMethod<FixtureKey, FixtureModule>>,
    nominals: HashMap<FixtureKey, DurableNominal<FixtureKey, FixtureModule>>,
    consts: HashMap<FixtureKey, DurableConst<FixtureKey, FixtureModule>>,
}

/// The in-memory durable fact source handed to the production body host. Facts
/// that were never seeded answer `None`, matching the provider contract that a
/// miss is authoritative — body analysis then reports the miss as an ordinary
/// diagnostic instead of inventing a fact.
#[derive(Clone)]
struct FixtureFactSource(Rc<FixtureFacts>);

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
struct UnconsultedFactProvider;

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

fn value_param(
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

/// Builder for one single-module provider fixture: explicit durable
/// declaration facts plus the production analysis helper.
struct ProviderFixture {
    facts: FixtureFacts,
}

impl ProviderFixture {
    fn new() -> Self {
        Self {
            facts: FixtureFacts {
                module_path: Arc::from("fixture/main.rue"),
                file: FileId::DEFAULT,
                ..FixtureFacts::default()
            },
        }
    }

    fn declare_function(
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

    fn declare_struct(
        &mut self,
        name: &str,
        fields: Vec<(&str, FixtureType)>,
        is_copy: bool,
    ) -> FixtureKey {
        let key = FixtureKey::nominal(name, StableDefinitionKind::Struct);
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
                body: DurableNominalBody::Struct {
                    fields: fields
                        .into_iter()
                        .map(|(field, ty)| (Arc::from(field), ty))
                        .collect(),
                    is_copy,
                    is_linear: false,
                },
            },
        );
        key
    }

    fn declare_method(
        &mut self,
        owner: &FixtureKey,
        name: &str,
        parameters: Vec<DurableSignatureParameter<FixtureKey, FixtureModule>>,
        result: FixtureType,
    ) -> FixtureKey {
        let key = FixtureKey::member(&owner.name, name, StableDefinitionKind::Method);
        self.facts.methods.insert(
            key.clone(),
            DurableMethod {
                receiver: SemanticImportType::Nominal(owner.clone()),
                parameters: parameters.into(),
                result,
                type_syntax: None,
                has_self: true,
                self_mode: SemanticParameterMode::Value,
                is_accessor: false,
            },
        );
        key
    }

    fn declare_const(
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

    /// Run the production provider body path over `source`, which must contain
    /// exactly the analyzed free function's declaration — mirroring the
    /// compiler's per-body plan, whose RIR bundle carries one declaration and
    /// nothing else. Every contextual fact must come from the seeded durable
    /// data, exactly as it does across the production provider boundary.
    fn analyze(
        &self,
        source: &str,
        function: &str,
    ) -> CompileResult<ProviderOrdinaryBody<FixtureKey, FixtureModule>> {
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
        let editor = astgen.finish_editor();
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
            FixtureKey::function(function),
            function,
            StableDefinitionKind::Function,
            None,
            Target::host().expect("host target resolves"),
            PreviewFeatures::new(),
            &ProviderWellKnownOptionFacts {
                nominals: Vec::new(),
                option_by_payload: Vec::new(),
            },
        )
    }
}

fn error_source_slice<'s>(source: &'s str, error: &rue_error::CompileError) -> &'s str {
    let span = error.span().expect("diagnostic carries its source span");
    &source[span.start as usize..span.end as usize]
}

// Migrated from `tests::test_analyze_addition`: ordinary expression typing on
// the production provider path.
#[test]
fn provider_body_types_integer_addition() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { 1 + 2 }", "main")
        .expect("addition body analyzes");

    let air = &body.function.air;
    assert_eq!(air.return_type(), crate::types::Type::I32);
    // Const(1) + Const(2) + Add + Ret = 4 instructions
    assert_eq!(air.len(), 4);
    let add = air.get(crate::AirRef::from_raw(2));
    assert!(matches!(add.data, crate::AirInstData::Add(_, _)));
    assert_eq!(add.ty, crate::types::Type::I32);
}

// Migrated from `tests::test_undefined_variable`: the diagnostic keeps its
// exact source span across the provider boundary.
#[test]
fn provider_body_reports_undefined_variable_with_exact_span() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 { missing + 1 }";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("undefined operand is rejected");

    assert!(
        matches!(&error.kind, ErrorKind::UndefinedVariable(name) if name == "missing"),
        "unexpected diagnostic: {error:?}"
    );
    assert_eq!(error_source_slice(source, &error), "missing");
}

// Migrated from `tests::test_use_after_move_error`: ownership diagnostics on
// the production provider path, with the callee crossing the boundary as an
// explicit durable signature fact.
#[test]
fn provider_body_reports_use_after_move() {
    let mut fixture = ProviderFixture::new();
    let non_copy = fixture.declare_struct("NonCopy", vec![("x", SemanticImportType::I32)], false);
    fixture.declare_function(
        "consume",
        vec![value_param("n", SemanticImportType::Nominal(non_copy))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let source = "fn main() -> i32 {
    let n = NonCopy { x: 42 };
    let consumed = consume(n);
    consumed + n.x
}";
    let error = fixture
        .analyze(source, "main")
        .map(|_| ())
        .expect_err("the moved value cannot be read again");

    assert!(
        matches!(&error.kind, ErrorKind::UseAfterMove { .. }),
        "unexpected diagnostic: {error:?}"
    );
    assert!(error.span().is_some(), "move diagnostic keeps its span");
}

// Migrated from `tests::test_copy_type_not_moved`: `@copy` metadata crosses
// the boundary inside the durable nominal fact.
#[test]
fn provider_body_copy_type_is_not_moved() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct("Point", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_function(
        "give",
        vec![value_param("p", SemanticImportType::Nominal(point))],
        SemanticImportType::I32,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let p = Point { x: 7 };
    let a = give(p);
    let b = give(p);
    a + b + p.x
}",
            "main",
        )
        .expect("a copy value survives repeated calls");
    assert!(body.warnings.is_empty(), "no incidental warnings expected");
}

// Provider-path counterpart of `tests::test_struct_field_type_resolution`
// (which keeps its whole-program type-pool assertions): aggregate
// construction with nested nominal fields resolved from durable facts.
#[test]
fn provider_body_resolves_nested_struct_field_types() {
    let mut fixture = ProviderFixture::new();
    let inner = fixture.declare_struct("Inner", vec![("x", SemanticImportType::I32)], true);
    fixture.declare_struct(
        "Outer",
        vec![("inner", SemanticImportType::Nominal(inner))],
        true,
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let o = Outer { inner: Inner { x: 42 } };
    o.inner.x
}",
            "main",
        )
        .expect("nested aggregate construction analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// Direct provider-path coverage of a method call: the member is resolved
// through the durable member lookup plus its durable method signature.
#[test]
fn provider_body_types_method_call() {
    let mut fixture = ProviderFixture::new();
    let point = fixture.declare_struct(
        "Point",
        vec![
            ("x", SemanticImportType::I32),
            ("y", SemanticImportType::I32),
        ],
        true,
    );
    fixture.declare_method(&point, "x_value", Vec::new(), SemanticImportType::I32);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    p.x_value()
}",
            "main",
        )
        .expect("method call analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// Provider-path counterpart of the body-local alias half of
// `tests::comptime_type_alias_filter_preserves_analysis_and_diagnostics`:
// a comptime type alias types a later binding on the production path.
#[test]
fn provider_body_reduces_local_comptime_type_alias() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let Direct = i32;
    let value: Direct = 40 + 2;
    value
}",
            "main",
        )
        .expect("comptime alias body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
}

// A comptime value crossing the boundary as a durable const fact.
#[test]
fn provider_body_reads_durable_const_value() {
    let mut fixture = ProviderFixture::new();
    let limit = fixture.declare_const(
        "LIMIT",
        SemanticImportType::I32,
        SemanticImportConstValue::Integer(40),
    );
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { LIMIT + 2 }", "main")
        .expect("durable const body analyzes");
    assert_eq!(body.function.air.return_type(), crate::types::Type::I32);
    assert!(
        body.referenced_values.contains(&limit),
        "the consulted const is recorded as a referenced value"
    );
}

// Provider-path counterpart of the local-alias half of
// `tests::direct_anonymous_type_alias_and_const_receive_authoritative_
// producers`: a body-local anonymous nominal is produced with a durable
// identity, and its member initialization type-checks.
#[test]
fn provider_body_produces_anonymous_nominal_identity() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze(
            "fn main() -> i32 {
    let T = struct { value: i32 };
    let holder: T = T { value: 42 };
    holder.value
}",
            "main",
        )
        .expect("anonymous nominal body analyzes");
    assert_eq!(body.produced_anonymous_nominals.len(), 1);
    let produced = &body.produced_anonymous_nominals[0];
    assert!(matches!(
        &produced.shape,
        super::provider_body_host::SemanticProducedAnonymousNominalShape::Struct { fields, .. }
            if fields.len() == 1 && &*fields[0].0 == "value"
    ));
}

// A fact that was never seeded fails closed: the miss surfaces as an ordinary
// visible diagnostic, never as an invented fact.
#[test]
fn provider_body_missing_function_fact_fails_closed() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let error = fixture
        .analyze("fn main() -> i32 { helper() }", "main")
        .map(|_| ())
        .expect_err("an unseeded callee cannot resolve");
    assert!(
        matches!(&error.kind, ErrorKind::UndefinedFunction(name) if name == "helper"),
        "unexpected diagnostic: {error:?}"
    );
}

// Warnings survive the provider boundary alongside the analyzed body.
#[test]
fn provider_body_preserves_unused_variable_warning() {
    let mut fixture = ProviderFixture::new();
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { let unused = 1; 2 }", "main")
        .expect("body with unused binding analyzes");
    assert!(
        body.warnings.iter().any(|warning| matches!(
            &warning.kind,
            rue_error::WarningKind::UnusedVariable(name) if name == "unused"
        )),
        "unexpected warnings: {:?}",
        body.warnings
    );
}

// Referenced definitions are reported exactly, so the compiler can register
// its dependency edges from the analysis result.
#[test]
fn provider_body_records_referenced_callee_definitions() {
    let mut fixture = ProviderFixture::new();
    let helper = fixture.declare_function("helper", Vec::new(), SemanticImportType::I32);
    fixture.declare_function("main", Vec::new(), SemanticImportType::I32);
    let body = fixture
        .analyze("fn main() -> i32 { helper() }", "main")
        .expect("callee body analyzes");
    assert!(
        body.referenced_definitions.contains(&helper),
        "the resolved callee is a referenced definition"
    );
    assert!(
        body.referenced_specializations.is_empty(),
        "an ordinary call requests no specialization"
    );
}

// Structural guard: the fixture helper drives the production provider entry
// point and never re-enters the retired whole-program `Sema` test drivers,
// and that entry point runs the one canonical ordinary-body engine.
#[test]
fn fixture_helper_drives_only_the_production_provider_entry_point() {
    let fixture_source = include_str!("provider_fixture_tests.rs");
    let entry = concat!("analyze_provider_", "ordinary_body(");
    assert!(
        fixture_source.contains(entry),
        "the fixture helper must call the production provider entry point"
    );
    for retired in [
        concat!("Sema::", "new_synthetic"),
        concat!("new_", "synthetic("),
        concat!("bind_declarations", "_for_test"),
        concat!("analyze_all", "_for_test"),
        concat!("analyze_", "all("),
        concat!("bind_", "declarations("),
    ] {
        assert!(
            !fixture_source.contains(retired),
            "the fixture must not re-enter the retired Sema driver: {retired}"
        );
    }
    // The entry point the helper calls runs the one canonical ordinary-body
    // engine. Matching the constructor and the resolved-signature entry
    // separately keeps the guard insensitive to formatting of the call chain.
    let provider_host = include_str!("provider_body_host.rs");
    assert!(
        provider_host.contains("OrdinaryBodyEngine::new"),
        "the provider host must construct the canonical ordinary-body engine"
    );
    assert!(
        provider_host.contains(".analyze_single_function_resolved("),
        "the provider entry point must run the engine's resolved ordinary-body analysis"
    );
}
