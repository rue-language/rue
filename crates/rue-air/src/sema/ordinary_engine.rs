//! The canonical ordinary-body algorithm over its two concrete storage hosts.
//!
//! [`OrdinaryBodyAnalysisHost`] names the state the algorithm actually needs;
//! the query-backed provider host is its one production implementation. [`OrdinaryBodyEngine`] only gives that one generic algorithm an
//! inherent-method receiver, which stable Rust cannot define on a trait. It
//! owns no state, representation conversion, cache, or alternative policy.

use ahash::{AHashMap, AHashSet};
use std::sync::Arc;
use std::time::Instant;

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind};
use rue_rir::{InstRef, Rir, RirParamMode, RirTypeSyntaxRef};
use rue_span::{FileId, Span};

use super::aggregate_resolution::is_accessible as aggregate_is_accessible;
use super::analysis::{ElementwiseConsumption, FirstClassStrSite, linear_not_consumed_error};
use super::anon_structs::{
    IssuedCanonicalArguments, IssuedFunctionInstanceKey, IssuedStableProducerId,
};
use super::context::{AnalysisContext, ParamIndex, ParamInfo};
use super::fact_mode::{
    ArrayLengthRequest, BodyAnalysisReadHost, ModulePrefixRequest, StructuredTypeSyntax,
    StructuredTypeSyntaxRequest, TypeSyntaxResult,
};
use super::info::{FunctionCallInfo, MethodCallInfo};
use super::{
    AnalyzedBodyOwnerEvent, AnalyzedFunction, BodyAnalysisWork, ConstInfo, ConstValue,
    DeclarationTypeDependencyKind, DeclarationTypeDependencySourceKind, FunctionInfo,
    InferenceContext, KnownSymbols, MethodInfo, ParamSlotModes,
};
use crate::inference::{InferType, LazyInferenceFacts};
use crate::inst::Air;
use crate::inst::{AirInst, AirInstData};
use crate::intern_pool::TypeInternPool;
use crate::types::{ArrayTypeId, EnumId, ModuleDef, ModuleId, StructId, Type};
use crate::{ParamRange, ParamRangeData};
use rue_target::Target;

pub(crate) fn reject_runtime_type_value(
    ty: Type,
    is_comptime: bool,
    span: Span,
) -> CompileResult<()> {
    if ty.is_comptime_type() && !is_comptime {
        return Err(CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: "a parameter of type `type` must be marked `comptime`".to_string(),
            },
            span,
        ));
    }
    Ok(())
}

/// Adjacent intervals inside one successful canonical expression analysis.
///
/// Provider-backed analysis publishes these fields with its existing body
/// completion event so detailed attribution does not add a second tracing
/// event per body.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExpressionAnalysisBreakdown {
    pub(crate) setup_ns: u64,
    pub(crate) inference_precompute_ns: u64,
    pub(crate) inference_precompute_structural_ns: u64,
    pub(crate) inference_precompute_eval_provider_ns: u64,
    pub(crate) precompute_work: super::comptime_eval::ComptimePrecomputeAttribution,
    pub(crate) constraint_generation_ns: u64,
    pub(crate) unification_resolution_ns: u64,
    pub(crate) air_emission_validation_ns: u64,
}

/// The neutral receiver contract consumed by the canonical ordinary-body engine.
///
/// This is intentionally narrower than the eventual full engine state. It
/// covers the body authority, semantic overlays, and exact point facts needed
/// by the ordinary expression engine. Transaction publication remains outside
/// this extraction.
pub(crate) trait OrdinaryBodyAnalysisHost: BodyAnalysisReadHost + Sized {
    type InferenceFacts<'a>: LazyInferenceFacts
    where
        Self: 'a;
    fn inference_facts<'a>(&'a self, ctx: &'a InferenceContext) -> Self::InferenceFacts<'a>;
    fn resolve_structured_type_syntax(
        &mut self,
        request: StructuredTypeSyntaxRequest<'_>,
    ) -> TypeSyntaxResult;
    fn resolve_type_module_prefix(
        &mut self,
        request: ModulePrefixRequest<'_>,
    ) -> CompileResult<(ModuleId, Option<FileId>, String)>;
    fn resolve_array_length(&mut self, request: ArrayLengthRequest<'_>) -> CompileResult<u64>;

    fn record_expression_analysis_breakdown(&mut self, _breakdown: ExpressionAnalysisBreakdown) {}

    fn body_param_data(&self, range: ParamRange) -> ParamRangeData;
    fn allocate_method_params(
        &mut self,
        names: impl IntoIterator<Item = Spur>,
        types: impl IntoIterator<Item = Type>,
        modes: impl IntoIterator<Item = RirParamMode>,
        comptime: impl IntoIterator<Item = bool>,
    ) -> ParamRange;
    fn install_anonymous_method(&mut self, key: (StructId, Spur), info: MethodInfo);
    fn known_symbols(&self) -> &KnownSymbols;

    fn strbuf_type(&self) -> Option<Type>;
    fn is_strbuf(&self, ty: Type) -> bool;
    fn types_equivalent(&self, left: Type, right: Type) -> bool;
    fn generated_structs(&self) -> &AHashMap<Spur, StructId>;

    fn body_interner(&self) -> &ThreadedRodeo;
    fn body_type_pool(&self) -> &TypeInternPool;
    fn struct_id_for_name(&self, name: Spur) -> Option<StructId>;
    fn generated_structs_mut(&mut self) -> &mut AHashMap<Spur, StructId>;
    fn generated_enums_mut(&mut self) -> &mut AHashMap<Spur, EnumId>;
    fn anonymous_struct_id(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<StructId>;
    fn anonymous_enum_id(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<EnumId>;
    fn anonymous_struct_identities_mut(
        &mut self,
    ) -> &mut AHashMap<super::anon_structs::IssuedAnonymousNominalKey, StructId>;
    fn anonymous_enum_identities_mut(
        &mut self,
    ) -> &mut AHashMap<super::anon_structs::IssuedAnonymousNominalKey, EnumId>;
    fn anonymous_digest_owner(
        &self,
        digest: u128,
    ) -> Option<&super::anon_structs::IssuedAnonymousNominalKey>;
    fn install_anonymous_digest_owner(
        &mut self,
        digest: u128,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
    );
    #[cfg(test)]
    fn forced_anonymous_digest(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> Option<u128>;
    fn install_canonical_anonymous_type(
        &mut self,
        ty: Type,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
    );
    fn anonymous_struct_methods_mut(
        &mut self,
    ) -> &mut AHashMap<StructId, Vec<super::AnonMethodSig>>;
    fn anonymous_struct_captures_mut(
        &mut self,
    ) -> &mut AHashMap<StructId, AHashMap<Spur, ConstValue>>;
    fn anonymous_struct_ids_mut(&mut self) -> &mut AHashSet<StructId>;
    fn anonymous_enum_ids_mut(&mut self) -> &mut AHashSet<EnumId>;
    fn canonical_type_instance(
        &self,
        ty: Type,
    ) -> Result<super::anon_structs::IssuedTypeInstanceKey, crate::SemanticBodyExportFailure>;
    fn canonical_argument_value(
        &self,
        value: ConstValue,
    ) -> Result<
        crate::CanonicalArgumentValue<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    >;
    fn function_identity(
        &self,
        symbol: Spur,
    ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure>;
    fn comptime_type_param_flags(&self, function: &FunctionCallInfo) -> Vec<bool>;
    fn function_param_type_syntax(
        &self,
        function: &FunctionCallInfo,
        param_index: usize,
    ) -> Option<StructuredTypeSyntax>;
    fn function_return_type_syntax(
        &self,
        function: &FunctionCallInfo,
    ) -> Option<StructuredTypeSyntax>;
    /// File whose declaration namespace owns the callable's retained type
    /// syntax. Provider-backed foreign callables override this; request-local
    /// callables use the body's span file.
    fn function_signature_root_file(&self, _function: &FunctionCallInfo) -> Option<FileId> {
        None
    }
    /// Reduce a comptime call whose body is not owned by this request. `None`
    /// means the callable is request-local and the ordinary evaluator should
    /// read its body; `Some` is the provider's exact reduction result and must
    /// never fall back to a request-local RIR handle.
    fn reduce_external_comptime_call(
        &mut self,
        _name: Spur,
        _callee_types: &AHashMap<Spur, Type>,
        _callee_values: &AHashMap<Spur, ConstValue>,
        _span: Span,
    ) -> Option<CompileResult<Option<ConstValue>>> {
        None
    }
    fn stable_definition_symbol_component(&self, token: &crate::SemanticDefinitionToken) -> String;
    fn stable_module_symbol_component(&self, token: &crate::SemanticModuleToken) -> String;
    fn resolve_body_type(&mut self, syntax: RirTypeSyntaxRef, span: Span) -> CompileResult<Type>;
    fn resolve_body_type_with_substitutions(
        &mut self,
        syntax: RirTypeSyntaxRef,
        span: Span,
        type_substitutions: Option<&AHashMap<Spur, Type>>,
        value_substitutions: Option<&AHashMap<Spur, ConstValue>>,
    ) -> CompileResult<Type>;
    fn replace_active_anonymous_producer(
        &mut self,
        producer: Option<(IssuedStableProducerId, IssuedCanonicalArguments)>,
    ) -> Option<(IssuedStableProducerId, IssuedCanonicalArguments)>;
    fn body_rir_ref(&self) -> &Rir;
    /// Whole-arena census of inline type-constructor head shapes (RUE-596),
    /// taken by the body RIR index build. Zero proves the inference
    /// precompute's reachability scan would collect no candidates.
    fn body_inline_ctor_head_candidates(&self) -> usize;
    fn active_anonymous_producer(
        &self,
    ) -> Option<&(IssuedStableProducerId, IssuedCanonicalArguments)>;
    fn body_declaration_type_observer(
        &self,
    ) -> Option<&(
        FileId,
        String,
        Option<String>,
        DeclarationTypeDependencySourceKind,
        DeclarationTypeDependencyKind,
    )>;
    fn body_analysis_work_mut(&mut self) -> &mut BodyAnalysisWork;
    fn record_resolved_declaration_type(&mut self, ty: Type);
    fn body_analysis_error_recovery(&self) -> bool;
    fn body_analysis_first_recovered_error(&self) -> Option<CompileError>;
    fn body_analysis_recovered_errors_mut(&mut self) -> &mut Vec<CompileError>;

    // Exact declaration/call data used by the ordinary expression engine.
    // These are host facts or request-local ledgers, not analyzer callbacks.
    fn function_info(&self, name: Spur) -> Option<FunctionCallInfo>;
    fn function_body_info(&self, name: Spur) -> Option<FunctionInfo>;
    fn value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn source_function_name(&self, name: Spur) -> Spur;
    fn resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur>;
    fn module_def(&self, module: ModuleId) -> ModuleDef;
    fn struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId>;
    fn builtin_struct(&self, name: Spur) -> Option<StructId>;
    fn target(&self) -> Target;
    fn builtin_arch_id(&self) -> Option<EnumId>;
    fn builtin_os_id(&self) -> Option<EnumId>;
    fn builtin_data_model_id(&self) -> Option<EnumId>;
    fn destructor_span(&self, struct_id: StructId) -> Option<Span>;
    fn infectious_linear_reason(&self, struct_id: StructId) -> Option<(String, String)>;
    fn well_known_option(&self, payload: Type) -> Option<Type>;
    fn set_anon_struct_type_subst(&mut self, struct_id: StructId, subst: AHashMap<Spur, Type>);
    fn anon_struct_type_subst(&self, struct_id: StructId) -> AHashMap<Spur, Type>;
    fn anon_struct_captured_values(&self, struct_id: StructId) -> AHashMap<Spur, ConstValue>;

    fn body_dependency_observer(&self) -> Option<AnalyzedBodyOwnerEvent>;
    fn record_body_named_dependency(&mut self, target: super::NamedConstDependencyTargetEvent);
    fn record_body_callable_dependency(&mut self, symbol: Spur);
    fn record_specialization_dependency(
        &mut self,
        identity: crate::FunctionInstanceKey<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    );

    fn intern_array_type(&mut self, element: Type, length: u64) -> ArrayTypeId;
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()>;
    fn declaration_binding_active(&self) -> bool;
    fn known_linear_during_binding(&self, ty: Type) -> Option<bool>;
    fn known_drop_glue_during_binding(&self, ty: Type) -> Option<bool>;
    fn comptime_type_call_depth(&self) -> usize;
    fn set_comptime_type_call_depth(&mut self, depth: usize);
    fn has_ctor_type_display(&self, ty: Type) -> bool;
    fn record_body_ctor_type_display(&mut self, ty: Type, display: String);
    fn trusted_try_producer(&self, ty: Type) -> Option<super::anon_structs::TrustedTryProducer>;
    fn resolve_canonical_import(
        &self,
        import_path: &str,
        span: Span,
    ) -> CompileResult<crate::ModuleId>;
    fn deferred_ownership_gates_mut(&mut self) -> &mut Vec<super::DeferredOwnershipGate>;
}

/// Inherent-method receiver for the generic ordinary-body algorithm.
///
/// The engine owns no analyzer representation and has no alternate semantic
/// algorithm. Both concrete hosts enter the same methods through this receiver.
pub(crate) struct OrdinaryBodyEngine<'h, H: OrdinaryBodyAnalysisHost> {
    storage: &'h mut H,
}

impl<'h, H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'h, H> {
    pub(crate) fn new(storage: &'h mut H) -> Self {
        Self { storage }
    }

    pub(crate) fn body_rir_ref(&self) -> &Rir {
        self.storage.body_rir_ref()
    }
    pub(crate) fn body_inline_ctor_head_candidates(&self) -> usize {
        self.storage.body_inline_ctor_head_candidates()
    }
    pub(crate) fn body_interner(&self) -> &ThreadedRodeo {
        self.storage.body_interner()
    }
    pub(crate) fn body_type_pool(&self) -> &TypeInternPool {
        self.storage.body_type_pool()
    }
    pub(crate) fn body_param_data(&self, range: ParamRange) -> ParamRangeData {
        self.storage.body_param_data(range)
    }
    pub(crate) fn known_symbols(&self) -> &KnownSymbols {
        self.storage.known_symbols()
    }
    pub(crate) fn active_anonymous_producer(
        &self,
    ) -> Option<&(IssuedStableProducerId, IssuedCanonicalArguments)> {
        self.storage.active_anonymous_producer()
    }
    pub(crate) fn function_info(&self, name: Spur) -> Option<FunctionCallInfo> {
        OrdinaryBodyAnalysisHost::function_info(self.storage, name)
    }
    pub(crate) fn function_body_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.storage.function_body_info(name)
    }
    pub(crate) fn function_identity(
        &self,
        symbol: Spur,
    ) -> Result<crate::SemanticDefinitionToken, crate::SemanticBodyExportFailure> {
        self.storage.function_identity(symbol)
    }
    pub(crate) fn value_const(&self, key: &(FileId, Spur)) -> Option<ConstInfo> {
        OrdinaryBodyAnalysisHost::value_const(self.storage, key.0, key.1)
    }
    pub(crate) fn source_function_name(&self, name: Spur) -> Spur {
        OrdinaryBodyAnalysisHost::source_function_name(self.storage, name)
    }
    pub(crate) fn resolve_function_name_local(&self, name: Spur, file: FileId) -> Option<Spur> {
        OrdinaryBodyAnalysisHost::resolve_function_name_local(self.storage, name, file)
    }
    pub(crate) fn module_def(&self, module: ModuleId) -> ModuleDef {
        OrdinaryBodyAnalysisHost::module_def(self.storage, module)
    }
    pub(crate) fn struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        OrdinaryBodyAnalysisHost::struct_in_file(self.storage, file, name)
    }
    pub(crate) fn builtin_struct(&self, name: Spur) -> Option<StructId> {
        OrdinaryBodyAnalysisHost::builtin_struct(self.storage, name)
    }
    pub(crate) fn target(&self) -> Target {
        self.storage.target()
    }
    pub(crate) fn builtin_arch_id(&self) -> Option<EnumId> {
        self.storage.builtin_arch_id()
    }
    pub(crate) fn builtin_os_id(&self) -> Option<EnumId> {
        self.storage.builtin_os_id()
    }
    pub(crate) fn builtin_data_model_id(&self) -> Option<EnumId> {
        self.storage.builtin_data_model_id()
    }
    pub(crate) fn destructor_span(&self, struct_id: StructId) -> Option<Span> {
        self.storage.destructor_span(struct_id)
    }
    pub(crate) fn infectious_linear_reason(&self, struct_id: StructId) -> Option<(String, String)> {
        self.storage.infectious_linear_reason(struct_id)
    }
    pub(crate) fn well_known_option(&self, payload: Type) -> Option<Type> {
        self.storage.well_known_option(payload)
    }
    pub(crate) fn set_anon_struct_type_subst(
        &mut self,
        struct_id: StructId,
        subst: AHashMap<Spur, Type>,
    ) {
        self.storage.set_anon_struct_type_subst(struct_id, subst)
    }
    pub(crate) fn body_dependency_observer(&self) -> Option<AnalyzedBodyOwnerEvent> {
        self.storage.body_dependency_observer()
    }
    pub(crate) fn record_body_named_dependency(
        &mut self,
        target: super::NamedConstDependencyTargetEvent,
    ) {
        self.storage.record_body_named_dependency(target)
    }
    pub(crate) fn record_body_callable_dependency(&mut self, symbol: Spur) {
        self.storage.record_body_callable_dependency(symbol)
    }
    pub(crate) fn record_body_method_dependency(&mut self, key: (StructId, Spur)) {
        let Some(info) = self.method_info(key) else {
            return;
        };
        let symbol = self.method_symbol(key.0, self.body_interner().resolve(&key.1), info.has_self);
        let symbol = self.body_interner().get_or_intern(&symbol);
        self.record_body_callable_dependency(symbol);
    }
    pub(crate) fn record_specialization_dependency(
        &mut self,
        identity: crate::FunctionInstanceKey<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
    ) {
        self.storage.record_specialization_dependency(identity)
    }
    pub(crate) fn body_analysis_recovered_errors_mut(&mut self) -> &mut Vec<CompileError> {
        self.storage.body_analysis_recovered_errors_mut()
    }
    pub(crate) fn get_or_create_array_type(&mut self, element: Type, length: u64) -> ArrayTypeId {
        self.storage.intern_array_type(element, length)
    }
    pub(crate) fn pre_create_array_types_from_infer_type(&mut self, ty: &InferType) {
        match ty {
            InferType::Array { element, length } => {
                self.pre_create_array_types_from_infer_type(element);
                let element = self.infer_type_to_concrete_type_for_key(element);
                if element != Type::ERROR && !Self::is_non_internable_element(element) {
                    self.get_or_create_array_type(element, *length);
                }
            }
            InferType::Concrete(_) | InferType::Var(_) | InferType::IntLiteral => {}
        }
    }
    pub(crate) fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        self.storage.require_preview(feature, what, span)
    }
    pub(crate) fn format_type_name(&self, ty: Type) -> String {
        ty.safe_name_with_pool(Some(self.body_type_pool()))
    }
    pub(crate) fn comptime_type_param_flags(&self, function: &FunctionCallInfo) -> Vec<bool> {
        self.storage.comptime_type_param_flags(function)
    }
    pub(crate) fn reduce_external_comptime_call(
        &mut self,
        name: Spur,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<CompileResult<Option<ConstValue>>> {
        self.storage
            .reduce_external_comptime_call(name, callee_types, callee_values, span)
    }
    pub(crate) fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        self.resolve_type_with_substitutions(type_sym, span, None, None)
    }
    pub(crate) fn resolve_type_with_ctx(
        &mut self,
        type_sym: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<Type> {
        self.resolve_type_with_substitutions(
            type_sym,
            span,
            Some(&ctx.comptime_type_vars),
            Some(&ctx.comptime_value_vars),
        )
    }

    pub(crate) fn rir_type_is_named(&self, syntax: RirTypeSyntaxRef, name: &str) -> bool {
        let arena = self.body_rir_ref().type_syntax();
        let Some(rue_rir::RirTypeSyntaxNode::Named(symbol)) = arena.node(syntax) else {
            return false;
        };
        arena
            .symbol(*symbol)
            .is_some_and(|symbol| self.body_interner().resolve(symbol) == name)
    }

    pub(crate) fn rir_type_named_symbol(&self, syntax: RirTypeSyntaxRef) -> Option<Spur> {
        let arena = self.body_rir_ref().type_syntax();
        let rue_rir::RirTypeSyntaxNode::Named(symbol) = arena.node(syntax)? else {
            return None;
        };
        arena.symbol(*symbol).copied()
    }

    pub(crate) fn render_rir_type(&self, syntax: RirTypeSyntaxRef) -> String {
        self.body_rir_ref()
            .type_syntax()
            .render_type_with(syntax, |symbol| self.body_interner().resolve(symbol))
            .unwrap_or_else(|| "<invalid structured type syntax>".to_owned())
    }

    pub(crate) fn resolve_rir_type(
        &mut self,
        syntax: RirTypeSyntaxRef,
        span: Span,
    ) -> CompileResult<Type> {
        self.storage.resolve_body_type(syntax, span)
    }

    pub(crate) fn resolve_rir_type_with_ctx(
        &mut self,
        syntax: RirTypeSyntaxRef,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<Type> {
        self.storage.resolve_body_type_with_substitutions(
            syntax,
            span,
            Some(&ctx.comptime_type_vars),
            Some(&ctx.comptime_value_vars),
        )
    }

    pub(crate) fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        syntax: RirTypeSyntaxRef,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<Type> {
        self.storage
            .resolve_body_type_with_substitutions(syntax, span, Some(type_subst), Some(value_subst))
            .ok()
    }
    pub(crate) fn extract_anon_method_sigs(
        &mut self,
        methods: &rue_rir::RirAnonStructMethodsRange,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
    ) -> Vec<super::AnonMethodSig> {
        fn resolve<H: OrdinaryBodyAnalysisHost>(
            engine: &mut OrdinaryBodyEngine<'_, H>,
            syntax: RirTypeSyntaxRef,
            type_subst: &AHashMap<Spur, Type>,
            value_subst: &AHashMap<Spur, ConstValue>,
            span: Span,
        ) -> super::AnonMethodType {
            use super::AnonMethodType as T;
            if engine.rir_type_is_named(syntax, "Self") {
                return T::SelfType;
            }
            engine
                .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                    syntax,
                    type_subst,
                    value_subst,
                    span,
                )
                .map_or_else(
                    || T::Syntax(engine.render_rir_type(syntax).into()),
                    T::Concrete,
                )
        }

        let method_refs = self.body_rir_ref().anon_struct_methods(methods).to_vec();
        let mut sigs = Vec::with_capacity(method_refs.len());
        for method_ref in method_refs {
            let method_inst = {
                let source = self.body_rir_ref().get(method_ref);
                rue_rir::Inst {
                    data: source.data.clone(),
                    span: source.span,
                }
            };
            if let rue_rir::InstData::FnDecl {
                name,
                params,
                return_type,
                has_self,
                self_mode,
                ..
            } = method_inst.data
            {
                let params = self.body_rir_ref().params(&params).to_vec();
                let param_types = params
                    .iter()
                    .map(|p| resolve(self, p.ty, type_subst, value_subst, method_inst.span))
                    .collect();
                let param_modes = params.iter().map(|p| p.mode).collect();
                let param_comptime = params.iter().map(|p| p.is_comptime).collect();
                sigs.push(super::AnonMethodSig {
                    name,
                    has_self,
                    self_mode,
                    param_types,
                    param_modes,
                    param_comptime,
                    return_type: resolve(
                        self,
                        return_type,
                        type_subst,
                        value_subst,
                        method_inst.span,
                    ),
                });
            }
        }
        sigs
    }

    pub(crate) fn find_method_own_comptime_type_param(
        &self,
        methods: &rue_rir::RirAnonStructMethodsRange,
    ) -> Option<(Span, String)> {
        for method_ref in self.body_rir_ref().anon_struct_methods(methods) {
            let method_inst = self.body_rir_ref().get(method_ref);
            if let rue_rir::InstData::FnDecl {
                name: method_name,
                params,
                ..
            } = &method_inst.data
            {
                for param in self.body_rir_ref().params(params) {
                    if param.is_comptime && self.rir_type_is_named(param.ty, "type") {
                        return Some((
                            method_inst.span,
                            self.body_interner().resolve(method_name).to_owned(),
                        ));
                    }
                }
            }
        }
        None
    }

    pub(crate) fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
    ) -> Option<()> {
        let method_refs = self.body_rir_ref().anon_struct_methods(methods).to_vec();
        let mut seen_methods = AHashSet::new();
        let mut staged = Vec::with_capacity(method_refs.len());
        for method_ref in method_refs {
            let method_inst = {
                let source = self.body_rir_ref().get(method_ref);
                rue_rir::Inst {
                    data: source.data.clone(),
                    span: source.span,
                }
            };
            let rue_rir::InstData::FnDecl {
                name: method_name,
                params,
                return_type,
                body,
                has_self,
                self_mode,
                self_is_mut,
                returns_borrow,
                ..
            } = method_inst.data
            else {
                continue;
            };
            let key = (struct_id, method_name);
            // Accessors are not supported on anonymous structs (ADR-0062
            // phase 1).
            if !seen_methods.insert(method_name) || self.has_method(key) || returns_borrow {
                return None;
            }
            let parameters = self.body_rir_ref().params(&params).to_vec();
            let mut parameter_types = Vec::with_capacity(parameters.len());
            for parameter in &parameters {
                let resolved = if self.rir_type_is_named(parameter.ty, "Self") {
                    Ok(struct_type)
                } else {
                    self.resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        parameter.ty,
                        type_subst,
                        value_subst,
                        method_inst.span,
                    )
                    .ok_or(())
                };
                parameter_types.push(resolved.ok()?);
            }
            let return_ty = if self.rir_type_is_named(return_type, "Self") {
                struct_type
            } else {
                self.resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                    return_type,
                    type_subst,
                    value_subst,
                    method_inst.span,
                )?
            };
            let param_range = self.storage.allocate_method_params(
                parameters.iter().map(|parameter| parameter.name),
                parameter_types,
                parameters.iter().map(|parameter| parameter.mode),
                parameters.iter().map(|parameter| parameter.is_comptime),
            );
            staged.push((
                key,
                MethodInfo {
                    struct_type,
                    has_self,
                    self_mode,
                    self_is_mut,
                    params: param_range,
                    return_type: return_ty,
                    body,
                    span: method_inst.span,
                    returns_borrow: false,
                },
            ));
        }
        for (key, info) in staged {
            self.storage.install_anonymous_method(key, info);
        }
        Some(())
    }

    fn resolve_type_with_substitutions(
        &mut self,
        type_sym: Spur,
        span: Span,
        type_substitutions: Option<&AHashMap<Spur, Type>>,
        value_substitutions: Option<&AHashMap<Spur, ConstValue>>,
    ) -> CompileResult<Type> {
        self.resolve_type_with_substitutions_in_file(
            type_sym,
            span.file_id,
            span,
            type_substitutions,
            value_substitutions,
        )
    }

    fn resolve_type_with_substitutions_in_file(
        &mut self,
        type_sym: Spur,
        root_file: FileId,
        span: Span,
        type_substitutions: Option<&AHashMap<Spur, Type>>,
        value_substitutions: Option<&AHashMap<Spur, ConstValue>>,
    ) -> CompileResult<Type> {
        let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
        let root = builder.push_named_type(type_sym).map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "failed to preserve a resolved type-name token: {failure:?}"
                )),
                span,
            )
        })?;
        let syntax = StructuredTypeSyntax {
            arena: builder.finish(),
            root,
        };
        self.storage
            .resolve_structured_type_syntax(StructuredTypeSyntaxRequest {
                syntax: &syntax,
                root_file,
                span,
                type_substitutions,
                value_substitutions,
            })
            .map_err(|failure| {
                super::typeck::semantic_type_syntax_compile_error(
                    self.body_interner(),
                    failure,
                    span,
                )
            })
    }
    pub(crate) fn type_carries_linear(&self, ty: Type) -> bool {
        self.body_type_pool().type_carries_linear(ty)
    }
    pub(crate) fn get_or_create_str_struct(&mut self, _span: Span) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};
        let type_sym = self.body_interner().get_or_intern("str");
        if let Some(struct_id) = self.storage.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }
        let ptr_type_id = self
            .storage
            .body_type_pool()
            .intern_ptr_const_from_type(Type::U8);
        let struct_def = StructDef {
            name: Arc::from("str"),
            fields: vec![
                StructField {
                    name: "ptr".to_owned(),
                    ty: Type::new_ptr_const(ptr_type_id),
                },
                StructField {
                    name: "len".to_owned(),
                    ty: Type::U64,
                },
            ],
            is_copy: true,
            is_linear: false,
            destructor: None,
            is_builtin: true,
            is_pub: true,
            file_id: FileId::new(0),
        };
        let (struct_id, _) = self
            .storage
            .body_type_pool()
            .register_struct(type_sym, struct_def);
        self.storage
            .generated_structs_mut()
            .insert(type_sym, struct_id);
        Ok(Type::new_struct(struct_id))
    }
    pub(crate) fn canonical_specialization_instance(
        &self,
        function_name: Spur,
        type_args: &[Type],
        value_args: &[ConstValue],
    ) -> Result<IssuedFunctionInstanceKey, crate::SemanticBodyExportFailure> {
        let arguments = IssuedCanonicalArguments {
            types: type_args
                .iter()
                .map(|ty| self.storage.canonical_type_instance(*ty))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            values: value_args
                .iter()
                .copied()
                .map(|value| self.storage.canonical_argument_value(value))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        };
        Ok(IssuedFunctionInstanceKey::Specialization {
            base: Box::new(IssuedFunctionInstanceKey::Definition(
                self.storage.function_identity(function_name)?,
            )),
            arguments,
        })
    }
    pub(crate) fn trusted_try_producer(
        &self,
        ty: Type,
    ) -> Option<super::anon_structs::TrustedTryProducer> {
        self.storage.trusted_try_producer(ty)
    }
    pub(crate) fn label_ctor_instantiation_site(err: CompileError, span: Span) -> CompileError {
        if matches!(err.kind, ErrorKind::ContainerElementIsLinear { .. }) {
            err.with_label("required by the type-constructor application here", span)
        } else {
            err
        }
    }
    pub(crate) fn resolve_builtin_struct_name(&self, name: Spur) -> Option<StructId> {
        self.builtin_struct(name)
    }
    pub(crate) fn resolve_substituted_param_type(
        &mut self,
        function: &FunctionCallInfo,
        param_index: usize,
        declared: Type,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Type> {
        if declared != Type::COMPTIME_TYPE {
            return Ok(declared);
        }
        let root_file = self
            .storage
            .function_signature_root_file(function)
            .unwrap_or(span.file_id);
        if let Some(syntax) = self
            .storage
            .function_param_type_syntax(function, param_index)
        {
            return self
                .storage
                .resolve_structured_type_syntax(StructuredTypeSyntaxRequest {
                    syntax: &syntax,
                    root_file,
                    span,
                    type_substitutions: Some(type_subst),
                    value_substitutions: Some(value_subst),
                })
                .map_err(|failure| {
                    super::typeck::semantic_type_syntax_compile_error(
                        self.body_interner(),
                        failure,
                        span,
                    )
                });
        }
        Err(CompileError::new(
            ErrorKind::InternalError(
                "generic parameter type is missing its structured declaration syntax".to_owned(),
            ),
            span,
        ))
    }
    pub(crate) fn resolve_substituted_return_type(
        &mut self,
        function: &FunctionCallInfo,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Type> {
        if function.return_type != Type::COMPTIME_TYPE {
            return Ok(function.return_type);
        }
        let root_file = self
            .storage
            .function_signature_root_file(function)
            .unwrap_or(span.file_id);
        if let Some(syntax) = self.storage.function_return_type_syntax(function) {
            return self
                .storage
                .resolve_structured_type_syntax(StructuredTypeSyntaxRequest {
                    syntax: &syntax,
                    root_file,
                    span,
                    type_substitutions: Some(type_subst),
                    value_substitutions: Some(value_subst),
                })
                .map_err(|failure| {
                    super::typeck::semantic_type_syntax_compile_error(
                        self.body_interner(),
                        failure,
                        span,
                    )
                });
        }
        Err(CompileError::new(
            ErrorKind::InternalError(
                "generic return type is missing its structured declaration syntax".to_owned(),
            ),
            span,
        ))
    }
    pub(crate) fn resolve_array_length(
        &mut self,
        length: &crate::types::ArrayLen,
        span: Span,
        values: Option<&AHashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        OrdinaryBodyAnalysisHost::resolve_array_length(
            self.storage,
            super::fact_mode::ArrayLengthRequest {
                length,
                span,
                value_substitutions: values,
            },
        )
    }
    pub(crate) fn resolve_type_module_prefix_in_file(
        &mut self,
        root_file: FileId,
        segments: &[&str],
        span: Span,
    ) -> CompileResult<(ModuleId, Option<FileId>, String)> {
        OrdinaryBodyAnalysisHost::resolve_type_module_prefix(
            self.storage,
            super::fact_mode::ModulePrefixRequest {
                root_file,
                segments,
                span,
            },
        )
    }
    pub(crate) fn resolve_qualified_type_name_in_file(
        &mut self,
        root_file: FileId,
        segments: &[&str],
        span: Span,
    ) -> CompileResult<Type> {
        let symbols = segments
            .iter()
            .map(|segment| self.body_interner().get_or_intern(segment))
            .collect::<Vec<_>>();
        let mut builder = rue_rir::RirTypeSyntaxBuilder::default();
        let root = builder.push_qualified_type(symbols).map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "failed to preserve a qualified type path: {failure:?}"
                )),
                span,
            )
        })?;
        let syntax = StructuredTypeSyntax {
            arena: builder.finish(),
            root,
        };
        self.storage
            .resolve_structured_type_syntax(StructuredTypeSyntaxRequest {
                syntax: &syntax,
                root_file,
                span,
                type_substitutions: None,
                value_substitutions: None,
            })
            .map_err(|failure| {
                super::typeck::semantic_type_syntax_compile_error(
                    self.body_interner(),
                    failure,
                    span,
                )
            })
    }
    pub(crate) fn module_binding(&self, key: &(FileId, Spur)) -> Option<ConstInfo> {
        self.call_facts().call_module_binding(key.0, key.1)
    }
    pub(crate) fn known_linear_during_binding(&self, ty: Type) -> Option<bool> {
        self.storage.known_linear_during_binding(ty)
    }
    pub(crate) fn known_drop_glue_during_binding(&self, ty: Type) -> Option<bool> {
        self.storage.known_drop_glue_during_binding(ty)
    }
    pub(crate) fn type_has_drop_glue(&self, ty: Type) -> bool {
        self.body_type_pool().type_needs_drop(ty)
    }
    pub(crate) fn type_ownership_depends_on_nominal(&self, ty: Type) -> bool {
        match ty.kind() {
            crate::types::TypeKind::Struct(_) | crate::types::TypeKind::Enum(_) => true,
            crate::types::TypeKind::Array(id) => {
                self.type_ownership_depends_on_nominal(self.body_type_pool().array_def(id).0)
            }
            _ => false,
        }
    }
    pub(crate) fn canonical_function_producer(
        &self,
        name: Spur,
        tys: &AHashMap<Spur, Type>,
        vals: &AHashMap<Spur, ConstValue>,
    ) -> Result<(IssuedStableProducerId, IssuedCanonicalArguments), crate::SemanticBodyExportFailure>
    {
        use crate::SemanticBodyExportFailure as F;
        let function = OrdinaryBodyAnalysisHost::function_info(self.storage, name)
            .ok_or(F::MissingStableIdentity)?;
        let flags = self.storage.comptime_type_param_flags(&function);
        let mut types = Vec::new();
        let mut values = Vec::new();
        for (index, (param_name, _, _, is_comptime)) in self
            .storage
            .body_param_data(function.params)
            .iter()
            .enumerate()
        {
            if !*is_comptime {
                continue;
            }
            if flags[index] {
                types.push(self.storage.canonical_type_instance(
                    *tys.get(param_name).ok_or(F::MissingStableIdentity)?,
                )?);
            } else {
                values.push(self.storage.canonical_argument_value(
                    *vals.get(param_name).ok_or(F::MissingStableIdentity)?,
                )?);
            }
        }
        let arguments = IssuedCanonicalArguments {
            types: types.into(),
            values: values.into(),
        };
        let base = IssuedFunctionInstanceKey::Definition(self.storage.function_identity(name)?);
        let function = if arguments.types.is_empty() && arguments.values.is_empty() {
            base
        } else {
            IssuedFunctionInstanceKey::Specialization {
                base: Box::new(base),
                arguments: arguments.clone(),
            }
        };
        Ok((
            IssuedStableProducerId::Function(Box::new(function)),
            arguments,
        ))
    }
    pub(crate) fn find_or_create_anon_struct(
        &mut self,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
        fields: &[crate::types::StructField],
        sigs: &[super::AnonMethodSig],
        captured: &AHashMap<Spur, ConstValue>,
    ) -> CompileResult<(Type, bool)> {
        if let Some(id) = self.storage.anonymous_struct_id(&identity) {
            return Ok((Type::new_struct(id), false));
        }
        let digest = self.stable_anonymous_identity_digest(&identity);
        self.guard_anonymous_digest_collision(digest, &identity)?;
        let id = self.storage.body_type_pool().reserve_struct_id();
        let name = super::anon_structs::anonymous_struct_name(digest);
        let name_spur = self.storage.body_interner().get_or_intern(&name);
        let drop_marker = self.storage.body_interner().get_or_intern("__drop");
        let has_destructor = sigs.iter().any(|sig| sig.name == drop_marker);
        let is_copy = !has_destructor
            && fields
                .iter()
                .all(|field| field.ty.is_copy_in_pool(self.storage.body_type_pool()));
        let destructor = has_destructor.then(|| Arc::from(format!("{name}.__drop").as_str()));
        let def = crate::types::StructDef {
            name: Arc::from(name.as_str()),
            fields: fields.to_vec(),
            is_copy,
            is_linear: false,
            destructor,
            is_builtin: false,
            is_pub: false,
            file_id: FileId::new(0),
        };
        self.storage
            .body_type_pool()
            .complete_struct_registration(id, name_spur, def);
        if !sigs.is_empty() {
            self.storage
                .anonymous_struct_methods_mut()
                .insert(id, sigs.to_vec());
        }
        if !captured.is_empty() {
            self.storage
                .anonymous_struct_captures_mut()
                .insert(id, captured.clone());
        }
        self.storage.generated_structs_mut().insert(name_spur, id);
        self.storage.anonymous_struct_ids_mut().insert(id);
        // The pool is the authority for consumers below sema (symbol spelling,
        // CFG destructor discovery, drop glue), which cannot see the sema-side
        // registry (RUE-1050).
        self.storage.body_type_pool().mark_anonymous_struct(id);
        let ty = Type::new_struct(id);
        self.storage
            .anonymous_struct_identities_mut()
            .insert(identity.clone(), id);
        self.storage.install_canonical_anonymous_type(ty, identity);
        Ok((ty, true))
    }
    pub(crate) fn find_or_create_anon_enum(
        &mut self,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
        names: &[String],
        payloads: &[Vec<Type>],
    ) -> CompileResult<Type> {
        if let Some(id) = self.storage.anonymous_enum_id(&identity) {
            return Ok(Type::new_enum(id));
        }
        let digest = self.stable_anonymous_identity_digest(&identity);
        self.guard_anonymous_digest_collision(digest, &identity)?;
        let name = super::anon_structs::anonymous_enum_name(digest);
        let name_spur = self.storage.body_interner().get_or_intern(&name);
        let def = crate::types::EnumDef {
            name: Arc::from(name.as_str()),
            variants: names.iter().map(|n| Arc::from(n.as_str())).collect(),
            variant_payloads: payloads.to_vec(),
            is_pub: false,
            file_id: FileId::new(0),
        };
        let (id, _) = self.storage.body_type_pool().register_enum(name_spur, def);
        self.storage.generated_enums_mut().insert(name_spur, id);
        self.storage.anonymous_enum_ids_mut().insert(id);
        // See `find_or_create_anon_struct` (RUE-1050).
        self.storage.body_type_pool().mark_anonymous_enum(id);
        let ty = Type::new_enum(id);
        self.storage
            .anonymous_enum_identities_mut()
            .insert(identity.clone(), id);
        self.storage.install_canonical_anonymous_type(ty, identity);
        Ok(ty)
    }
    fn stable_anonymous_identity_digest(
        &self,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> u128 {
        #[cfg(test)]
        if let Some(digest) = self.storage.forced_anonymous_digest(identity) {
            return digest;
        }
        let stable: crate::AnonymousNominalKey<String, String> = identity
            .try_map_identities::<String, String, std::convert::Infallible>(
                &|token| Ok(self.storage.stable_definition_symbol_component(token)),
                &|token| Ok(self.storage.stable_module_symbol_component(token)),
            )
            .expect("anonymous identity relocation is infallible");
        crate::stable_digest::stable_anonymous_identity_digest(&stable)
    }
    fn guard_anonymous_digest_collision(
        &mut self,
        digest: u128,
        identity: &super::anon_structs::IssuedAnonymousNominalKey,
    ) -> CompileResult<()> {
        match self.storage.anonymous_digest_owner(digest) {
            Some(existing) if existing == identity => Ok(()),
            Some(existing) => Err(CompileError::without_span(ErrorKind::InternalError(
                format!(
                    "anonymous-symbol digest collision: existing owner {:?}, colliding key {:?} (digest {digest:032x})",
                    existing, identity,
                ),
            ))),
            None => {
                self.storage
                    .install_anonymous_digest_owner(digest, identity.clone());
                Ok(())
            }
        }
    }
    pub(crate) fn has_ctor_type_display(&self, ty: Type) -> bool {
        self.storage.has_ctor_type_display(ty)
    }
    pub(crate) fn record_body_ctor_type_display(&mut self, ty: Type, display: String) {
        self.storage.record_body_ctor_type_display(ty, display)
    }
    pub(crate) fn deferred_ownership_gates_mut(
        &mut self,
    ) -> &mut Vec<super::DeferredOwnershipGate> {
        self.storage.deferred_ownership_gates_mut()
    }
    pub(crate) fn resolve_canonical_import(
        &self,
        path: &str,
        span: Span,
    ) -> CompileResult<crate::ModuleId> {
        self.storage.resolve_canonical_import(path, span)
    }
    pub(crate) fn resolve_module_binding_in_file(
        &self,
        file: FileId,
        name: Spur,
    ) -> CompileResult<Option<ConstInfo>> {
        Ok(self.module_binding(&(file, name)))
    }
    pub(crate) fn reject_slice_escape(
        &self,
        syntax: RirTypeSyntaxRef,
        span: Span,
        kind: ErrorKind,
    ) -> CompileResult<()> {
        if matches!(
            self.body_rir_ref().type_syntax().node(syntax),
            Some(rue_rir::RirTypeSyntaxNode::Slice { .. })
        ) {
            self.require_preview(
                rue_error::PreviewFeature::Slices,
                "the slice type `[T]`",
                span,
            )?;
            return Err(CompileError::new(kind, span));
        }
        Ok(())
    }
    pub(crate) fn declaration_binding_active(&self) -> bool {
        self.storage.declaration_binding_active()
    }
    pub(crate) fn comptime_type_call_depth(&self) -> usize {
        self.storage.comptime_type_call_depth()
    }
    pub(crate) fn set_comptime_type_call_depth(&mut self, depth: usize) {
        self.storage.set_comptime_type_call_depth(depth)
    }
    pub(crate) fn generated_structs(&self) -> &AHashMap<Spur, StructId> {
        self.storage.generated_structs()
    }
    pub(crate) fn body_analysis_work_mut(&mut self) -> &mut BodyAnalysisWork {
        self.storage.body_analysis_work_mut()
    }
    pub(crate) fn body_analysis_error_recovery(&self) -> bool {
        self.storage.body_analysis_error_recovery()
    }
    pub(crate) fn is_strbuf(&self, ty: Type) -> bool {
        self.storage.is_strbuf(ty)
    }
    pub(crate) fn strbuf_type(&self) -> Option<Type> {
        self.storage.strbuf_type()
    }
    pub(crate) fn record_resolved_declaration_type(&mut self, ty: Type) {
        self.storage.record_resolved_declaration_type(ty)
    }
    pub(crate) fn types_equivalent(&self, left: Type, right: Type) -> bool {
        self.storage.types_equivalent(left, right)
    }
    pub(crate) fn aggregate_facts(&self) -> &H {
        self.storage
    }
    pub(crate) fn is_accessible(
        &self,
        accessing_file_id: FileId,
        target_file_id: FileId,
        is_pub: bool,
    ) -> bool {
        aggregate_is_accessible(
            self.aggregate_facts(),
            accessing_file_id,
            target_file_id,
            is_pub,
        )
    }
    pub(crate) fn check_unqualified_visibility(
        &self,
        item_kind: &str,
        name: &str,
        defining_file_id: FileId,
        is_pub: bool,
        span: Span,
    ) -> CompileResult<()> {
        if self.is_accessible(span.file_id, defining_file_id, is_pub) {
            return Ok(());
        }
        let defining_file = self
            .aggregate_facts()
            .aggregate_file_path(defining_file_id)
            .unwrap_or("<unknown>")
            .to_string();
        Err(CompileError::new(
            ErrorKind::PrivateUnqualifiedAccess(Box::new(
                rue_error::PrivateUnqualifiedAccessData {
                    item_kind: item_kind.to_string(),
                    name: name.to_string(),
                    defining_file,
                },
            )),
            span,
        )
        .with_help(format!(
            "`{name}` is not marked `pub`; private items are only visible within their defining directory"
        )))
    }
    pub(crate) fn call_facts(&self) -> &H {
        self.storage
    }
    pub(crate) fn method_info(&self, key: (StructId, Spur)) -> Option<MethodCallInfo> {
        self.call_facts().call_method_info(key.0, key.1)
    }
    pub(crate) fn has_method(&self, key: (StructId, Spur)) -> bool {
        self.method_info(key).is_some()
    }
    pub(crate) fn symbol_type_name(&self, struct_id: StructId) -> String {
        self.body_type_pool().struct_symbol_name(struct_id)
    }
    pub(crate) fn method_symbol(
        &self,
        struct_id: StructId,
        method: &str,
        has_self: bool,
    ) -> String {
        let type_name = self.symbol_type_name(struct_id);
        if has_self {
            format!("{type_name}.{method}")
        } else {
            format!("{type_name}::{method}")
        }
    }
    pub(crate) fn is_type_copy(&self, ty: Type) -> bool {
        match ty.kind() {
            crate::types::TypeKind::I8
            | crate::types::TypeKind::I16
            | crate::types::TypeKind::I32
            | crate::types::TypeKind::I64
            | crate::types::TypeKind::U8
            | crate::types::TypeKind::U16
            | crate::types::TypeKind::U32
            | crate::types::TypeKind::U64
            | crate::types::TypeKind::Bool
            | crate::types::TypeKind::Unit
            | crate::types::TypeKind::Never
            | crate::types::TypeKind::Error
            | crate::types::TypeKind::Module(_)
            | crate::types::TypeKind::ComptimeType
            | crate::types::TypeKind::PtrConst(_)
            | crate::types::TypeKind::PtrMut(_) => true,
            crate::types::TypeKind::Enum(id) => self
                .body_type_pool()
                .enum_def(id)
                .variant_payloads
                .iter()
                .flatten()
                .all(|&ty| self.is_type_copy(ty)),
            crate::types::TypeKind::Struct(id) => self.body_type_pool().struct_def(id).is_copy,
            crate::types::TypeKind::Array(id) => {
                let (element, _) = self.body_type_pool().array_def(id);
                self.is_type_copy(element)
            }
        }
    }
    pub(crate) fn types_compatible(&self, found: Type, expected: Type) -> bool {
        found.is_never() || found.is_error() || self.types_equivalent(found, expected)
    }
    pub(crate) fn function_returns_type(&self, function: &FunctionCallInfo) -> bool {
        function.returns_type
    }
    pub(crate) fn is_non_internable_element(ty: Type) -> bool {
        matches!(
            ty.kind(),
            crate::types::TypeKind::ComptimeType | crate::types::TypeKind::Module(_)
        )
    }
    pub(crate) fn infer_type_to_type(&mut self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,
            InferType::IntLiteral => Type::I32,
            InferType::Array { element, length } => {
                let element = self.infer_type_to_type(element);
                if element == Type::ERROR || Self::is_non_internable_element(element) {
                    Type::ERROR
                } else {
                    Type::new_array(self.get_or_create_array_type(element, *length))
                }
            }
        }
    }
    pub(crate) fn infer_type_to_concrete_type_for_key(&mut self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,
            InferType::IntLiteral => Type::I32,
            InferType::Array { element, length } => {
                let element = self.infer_type_to_concrete_type_for_key(element);
                if element == Type::ERROR || Self::is_non_internable_element(element) {
                    Type::ERROR
                } else {
                    Type::new_array(self.get_or_create_array_type(element, *length))
                }
            }
        }
    }
    pub(crate) fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty.kind() {
            crate::types::TypeKind::Array(id) => {
                let (element, length) = self.body_type_pool().array_def(id);
                InferType::Array {
                    element: Box::new(self.type_to_infer_type(element)),
                    length,
                }
            }
            _ => InferType::Concrete(ty),
        }
    }
    pub(crate) fn inference_facts<'a>(
        &'a self,
        ctx: &'a InferenceContext,
    ) -> H::InferenceFacts<'a> {
        self.storage.inference_facts(ctx)
    }

    pub(crate) fn slice_element_type(&self, ty: Type) -> Option<Type> {
        let crate::types::TypeKind::Struct(struct_id) = ty.kind() else {
            return None;
        };
        let def = self.body_type_pool().struct_def(struct_id);
        if crate::types::is_slice_struct_name(&def.name)
            || crate::types::is_string_view_struct_name(&def.name)
        {
            if let crate::types::TypeKind::PtrConst(ptr_id) = def.fields[0].ty.kind() {
                return Some(self.body_type_pool().ptr_const_def(ptr_id));
            }
        }
        None
    }

    pub(crate) fn validate_explicit_call_modes<A>(
        &self,
        args: A,
        expected_modes: impl ExactSizeIterator<Item = RirParamMode>,
    ) -> rue_error::CompileResult<()>
    where
        A: IntoIterator,
        A::IntoIter: ExactSizeIterator,
        A::Item: std::ops::Deref<Target = rue_rir::RirCallArg>,
    {
        let args = args.into_iter();
        assert_eq!(args.len(), expected_modes.len());
        for (arg, expected_mode) in args.zip(expected_modes) {
            let arg = &*arg;
            use rue_rir::RirArgMode;
            match (expected_mode, arg.mode) {
                (RirParamMode::Inout, RirArgMode::Inout)
                | (RirParamMode::Borrow, RirArgMode::Borrow)
                | (RirParamMode::Normal, RirArgMode::Normal) => {}
                (RirParamMode::Inout, _) => {
                    return Err(CompileError::new(
                        ErrorKind::InoutKeywordMissing,
                        self.body_rir_ref().get(arg.value).span,
                    ));
                }
                (RirParamMode::Borrow, _) => {
                    return Err(CompileError::new(
                        ErrorKind::BorrowKeywordMissing,
                        self.body_rir_ref().get(arg.value).span,
                    ));
                }
                (RirParamMode::Normal, actual) => {
                    let mode = match actual {
                        RirArgMode::Inout => "inout",
                        RirArgMode::Borrow => "borrow",
                        RirArgMode::Normal => unreachable!(),
                    };
                    return Err(CompileError::new(ErrorKind::UnexpectedCallArgumentMode { mode }, self.body_rir_ref().get(arg.value).span)
                        .with_help(format!("remove the `{mode}` keyword; this argument is passed without an explicit mode")));
                }
            }
        }
        Ok(())
    }

    /// Analyze a function whose body-exact return type and canonical parameter
    /// facts are already resolved in this engine's type and parameter authority.
    ///
    /// Provider-backed body analysis mints those facts while selecting the
    /// current callable. Re-resolving the explicit parameter spellings would
    /// duplicate nominal registration and type construction without adding a
    /// distinct correctness check; the body query already depends on the exact
    /// canonical parsed signature projection. The caller still resolves the declared return
    /// spelling because a call-site signature may expose a coercion-normalized
    /// type instead of the exact type checked inside the body.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_single_function_resolved(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        ret_type: Type,
        param_info: Vec<(Spur, Type, RirParamMode, bool)>,
        body: InstRef,
        span: Span,
        allow_unused_variable: bool,
        allow_unreachable_code: bool,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        for (_, ty, _, is_comptime) in &param_info {
            reject_runtime_type_value(*ty, *is_comptime, span)?;
        }
        let function_symbol = self.storage.body_interner().get_or_intern(fn_name);
        let producer = self
            .canonical_function_producer(function_symbol, &AHashMap::new(), &AHashMap::new())
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical producer for '{fn_name}': {failure:?}"
                    )),
                    span,
                )
            })?;
        let crate::StableProducerId::Function(identity) = &producer.0 else {
            unreachable!("a callable body producer is always a function")
        };
        let identity = (**identity).clone();
        let previous_producer = self
            .storage
            .replace_active_anonymous_producer(Some(producer));
        let analysis = self.analyze_function(
            infer_ctx,
            ret_type,
            &param_info,
            body,
            allow_unused_variable,
        );
        self.storage
            .replace_active_anonymous_producer(previous_producer);
        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            local_atoms,
            ref_fns,
            ref_meths,
        ) = analysis?;
        Ok((
            AnalyzedFunction {
                identity,
                callable_kind: crate::AnalyzedCallableKind::Ordinary,
                ordinary_owner: None,
                name: fn_name.to_owned(),
                implicit_drop_source: None,
                air: crate::ValidatedAir::from_semantic_air_with_symbols(
                    air,
                    self.storage.body_type_pool(),
                    self.storage.body_interner(),
                )?,
                local_atoms,
                num_locals,
                num_param_slots,
                param_modes,
                allow_unreachable_code,
            },
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ))
    }

    /// Analyze a named member from its already-resolved canonical signature.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_named_method_resolved(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        return_type: Type,
        params: Vec<(Spur, Type, RirParamMode, bool)>,
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
        self_mode: RirParamMode,
        self_is_mut: bool,
        returns_borrow: bool,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        let symbol = self.storage.body_interner().get_or_intern(full_name);
        let identity = crate::FunctionInstanceKey::Definition(
            self.storage.function_identity(symbol).map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical producer for method '{full_name}': {failure:?}"
                    )),
                    span,
                )
            })?,
        );
        let self_name = self.storage.body_interner().get_or_intern("self");
        let mut resolved_params = Vec::with_capacity(params.len() + usize::from(has_self));
        if has_self {
            resolved_params.push((self_name, struct_type, self_mode, false));
        }
        resolved_params.extend(params);
        self.analyze_method_with_identity_kind_resolved(
            infer_ctx,
            identity,
            full_name,
            return_type,
            resolved_params,
            body,
            span,
            struct_type,
            self_is_mut,
            false,
            returns_borrow,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_method_with_identity_kind_resolved(
        &mut self,
        infer_ctx: &InferenceContext,
        identity: crate::FunctionInstanceKey<
            crate::SemanticDefinitionToken,
            crate::SemanticModuleToken,
        >,
        full_name: &str,
        return_type: Type,
        resolved_params: Vec<(Spur, Type, RirParamMode, bool)>,
        body: InstRef,
        span: Span,
        struct_type: Type,
        self_is_mut: bool,
        is_destructor: bool,
        is_accessor: bool,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        for (_, ty, _, is_comptime) in &resolved_params {
            reject_runtime_type_value(*ty, *is_comptime, span)?;
        }
        let producer = (
            crate::StableProducerId::Function(Box::new(identity.clone())),
            crate::CanonicalArguments::default(),
        );
        let previous = self
            .storage
            .replace_active_anonymous_producer(Some(producer));
        let struct_id = struct_type
            .as_struct()
            .expect("method receiver must be a struct");
        let self_type_name = self.storage.body_interner().get_or_intern("Self");
        let mut type_subst = self.storage.anon_struct_type_subst(struct_id);
        type_subst.insert(self_type_name, struct_type);
        let captured_values = self.storage.anon_struct_captured_values(struct_id);
        let analysis = self.analyze_function_internal(
            infer_ctx,
            return_type,
            &resolved_params,
            body,
            Some(&type_subst),
            Some(&captured_values),
            is_destructor,
            false,
            self_is_mut,
            is_accessor,
        );
        self.storage.replace_active_anonymous_producer(previous);
        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            strings,
            local_atoms,
            referenced_functions,
            referenced_methods,
        ) = analysis?;
        if is_destructor {
            super::analysis::reject_self_move_in_destructor(&air, full_name)?;
            air.clear_param_drops();
        }
        Ok((
            AnalyzedFunction {
                identity,
                callable_kind: if is_destructor {
                    crate::AnalyzedCallableKind::Destructor
                } else if is_accessor {
                    crate::AnalyzedCallableKind::Accessor
                } else {
                    crate::AnalyzedCallableKind::Ordinary
                },
                ordinary_owner: None,
                name: full_name.to_owned(),
                implicit_drop_source: is_destructor
                    .then_some(super::ImplicitDropDependencySourceEvent::Anonymous),
                air: crate::ValidatedAir::from_semantic_air_with_symbols(
                    air,
                    self.storage.body_type_pool(),
                    self.storage.body_interner(),
                )?,
                local_atoms,
                num_locals,
                num_param_slots,
                param_modes,
                allow_unreachable_code: false,
            },
            warnings,
            strings,
            referenced_functions,
            referenced_methods,
        ))
    }

    pub(crate) fn analyze_named_destructor(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        body: InstRef,
        span: Span,
        struct_type: Type,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        let self_name = self.storage.body_interner().get_or_intern("self");
        let params = [(self_name, struct_type, RirParamMode::Normal, false)];
        let identity_token = self.storage.function_identity(
            self.storage.body_interner().get_or_intern(full_name),
        ).map_err(|failure| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "failed to issue canonical producer for destructor '{full_name}': {failure:?}"
                )),
                span,
            )
        })?;
        let identity = crate::FunctionInstanceKey::Definition(identity_token);
        let producer = (
            crate::StableProducerId::Function(Box::new(identity.clone())),
            crate::CanonicalArguments::default(),
        );
        let previous = self
            .storage
            .replace_active_anonymous_producer(Some(producer));
        let analysis = self.analyze_function_internal(
            infer_ctx,
            Type::UNIT,
            &params,
            body,
            None,
            None,
            true,
            false,
            false,
            false,
        );
        self.storage.replace_active_anonymous_producer(previous);
        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            strings,
            local_atoms,
            referenced_functions,
            referenced_methods,
        ) = analysis?;
        super::analysis::reject_self_move_in_destructor(&air, full_name)?;
        air.clear_param_drops();
        Ok((
            AnalyzedFunction {
                identity,
                callable_kind: crate::AnalyzedCallableKind::Destructor,
                ordinary_owner: None,
                name: full_name.to_owned(),
                implicit_drop_source: None,
                air: crate::ValidatedAir::from_semantic_air_with_symbols(
                    air,
                    self.storage.body_type_pool(),
                    self.storage.body_interner(),
                )?,
                local_atoms,
                num_locals,
                num_param_slots,
                param_modes,
                allow_unreachable_code: false,
            },
            warnings,
            strings,
            referenced_functions,
            referenced_methods,
        ))
    }

    /// Analyze one instruction through the canonical ordinary dispatcher and
    /// its ordinary expression helper families.
    pub(crate) fn analyze_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        allow_unused_variable: bool,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            None,
            None,
            false,
            allow_unused_variable,
            false,
            false,
        )
    }

    pub(crate) fn analyze_specialized_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        type_subst: &AHashMap<Spur, Type>,
        value_subst: &AHashMap<Spur, ConstValue>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            Some(type_subst),
            Some(value_subst),
            false,
            false,
            false,
            false,
        )
    }

    pub(crate) fn analyze_function_internal(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode, bool)],
        body: InstRef,
        type_subst: Option<&AHashMap<Spur, Type>>,
        value_subst: Option<&AHashMap<Spur, ConstValue>>,
        is_destructor: bool,
        allow_unused_variable: bool,
        self_is_mut: bool,
        is_accessor: bool,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        ParamSlotModes,
        Vec<CompileWarning>,
        Vec<String>,
        Vec<crate::LocalAtomRecord<crate::SemanticDefinitionToken, crate::SemanticModuleToken>>,
        AHashSet<Spur>,
        AHashSet<(StructId, Spur)>,
    )> {
        let expression_setup_started = Instant::now();
        let mut air = Air::new(return_type);

        // Preview gate (RUE-15 / ADR-0037): a `borrow self` / `inout self`
        // receiver lowers to a synthetic `self` parameter carrying a non-Normal
        // mode. `self` can never be a user-written parameter name (it is a
        // dedicated keyword), so this is a reliable single chokepoint for
        // every method body — named or anonymous struct — that actually
        // reaches analysis. By-value `self` (Normal) and destructors are
        // unaffected.
        let self_sym = self.body_interner().get_or_intern("self");
        if let Some((_, _, mode, _)) = params
            .iter()
            .find(|(name, _, mode, _)| *name == self_sym && *mode != RirParamMode::Normal)
        {
            debug_assert!(matches!(mode, RirParamMode::Inout | RirParamMode::Borrow));
        }

        // Classify and total every parameter before allocating per-slot mode
        // metadata. This makes a cumulatively oversized signature fail with
        // E0907 instead of first attempting a displacement-sized allocation
        // (RUE-780).
        let mut num_param_slots = 0_u32;
        let mut param_layouts = Vec::with_capacity(params.len());
        for (pname, ptype, mode, _) in params.iter() {
            // Only the synthetic `self` entry of a `mut self` method can be a
            // mutable by-value binding; ordinary parameters have no `mut`
            // form.
            let is_mut_binding = self_is_mut && *pname == self_sym && *mode == RirParamMode::Normal;
            // Inout and Borrow parameters are passed by reference.
            // Comptime parameters are VALUE params (like `comptime n: i32`), passed by value.
            // Normal parameters are passed by value.
            //
            // Exception (ADR-0043, RUE-322): a slice parameter `borrow s: [T]`
            // is itself a two-word fat pointer `{ptr, len}`. The `borrow`/`inout`
            // keyword marks shared-vs-exclusive access, not an extra level of
            // indirection, so the slice value is passed BY VALUE through the
            // existing multi-slot aggregate ABI (2 slots) rather than as a
            // pointer-to-slice. The call site materializes the fat pointer.
            //
            // `str` intentionally diverges for `inout`: it is first-class and
            // reassignable, so assigning to an `inout str` parameter must
            // rebind the caller's `{ptr, len}` fat pointer. That requires a
            // normal by-reference parameter slot; otherwise AIR emits
            // ParamStore but codegen sees a by-value param slot and panics
            // (RUE-385).
            let is_slice = self.slice_element_type(*ptype).is_some();
            let is_inout_str = *mode == RirParamMode::Inout && self.is_str_struct(*ptype);
            // Exact `borrow Str(N)` / `inout Str(N)` parameters retain their
            // nominal fixed-capacity type and use the ordinary one-pointer
            // by-reference ABI. Only actual slice views (including bare
            // `borrow str`) use the materialized two-slot by-value ABI.
            let is_exact_str_fixed_ref = matches!(mode, RirParamMode::Borrow | RirParamMode::Inout)
                && self.is_str_fixed_struct(*ptype);
            let is_slice_by_value = is_slice && !is_inout_str && !is_exact_str_fixed_ref;
            let is_by_ref = (*mode == RirParamMode::Inout || *mode == RirParamMode::Borrow)
                && !is_slice_by_value;
            let slot_count = if is_by_ref {
                // By-ref parameters are always 1 slot (pointer)
                1
            } else {
                // A by-value parameter materializes the whole object in the
                // frame: reject an oversized type (E0906, RUE-561).
                self.require_layout_slots(*ptype, self.body_rir_ref().get(body).span)?
            };
            self.reserve_frame_slots(
                &mut num_param_slots,
                slot_count,
                self.body_rir_ref().get(body).span,
            )?;
            param_layouts.push((is_by_ref, is_mut_binding, slot_count));
        }

        let mut param_vec: Vec<ParamInfo> = Vec::with_capacity(params.len());
        let mut param_by_ref: Vec<bool> = Vec::with_capacity(num_param_slots as usize);
        let mut param_writable: Vec<bool> = Vec::with_capacity(num_param_slots as usize);

        // Publish the already-validated offsets and per-slot modes.
        let mut next_abi_slot = 0_u32;
        for ((pname, ptype, mode, is_comptime), (is_by_ref, is_mut_binding, slot_count)) in
            params.iter().zip(param_layouts)
        {
            param_vec.push(ParamInfo {
                name: *pname,
                abi_slot: next_abi_slot,
                ty: *ptype,
                mode: *mode,
                is_comptime: *is_comptime,
                is_mut: is_mut_binding,
            });
            param_by_ref.resize(param_by_ref.len() + slot_count as usize, is_by_ref);
            // "Writable" means the body may store to the slot: `inout`
            // (by-ref, written back to the caller) or a `mut self` receiver
            // (by-value, callee-local).
            param_writable.resize(
                param_writable.len() + slot_count as usize,
                *mode == RirParamMode::Inout || is_mut_binding,
            );
            next_abi_slot += slot_count;
        }
        debug_assert_eq!(next_abi_slot, num_param_slots);
        let param_modes = ParamSlotModes::new(param_by_ref, param_writable);

        // The callee owns its pass-by-value (Normal) parameters and must drop
        // them at exit unless they are moved out (RUE-61). Inout/borrow params
        // stay owned by the caller; comptime params are substituted away.
        // Destructors clear this list after analysis (see the destructor path).
        air.set_param_drops(
            param_vec
                .iter()
                .filter(|p| {
                    p.mode == RirParamMode::Normal && !p.is_comptime && p.abi_slot < num_param_slots
                })
                .map(|p| (p.abi_slot, p.ty))
                .collect(),
        );
        let param_index = ParamIndex::new(&param_vec);

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let expression_setup_ns =
            u64::try_from(expression_setup_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let (resolved_types, inference_breakdown) = self.run_type_inference(
            infer_ctx,
            return_type,
            params,
            body,
            type_subst,
            value_subst,
        )?;
        let air_emission_started = Instant::now();

        // Create analysis context with resolved types
        // If type_subst is provided, initialize comptime_type_vars with the substitutions
        // so that type parameters can be resolved during struct initialization.
        let comptime_type_vars = type_subst.map(|s| s.clone()).unwrap_or_else(AHashMap::new);
        let comptime_value_vars = value_subst.map(|s| s.clone()).unwrap_or_else(AHashMap::new);
        let (canonical_producer, canonical_producer_arguments) =
            self.active_anonymous_producer().cloned().ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(
                        "body analysis started without a canonical producer identity".into(),
                    ),
                    self.body_rir_ref().get(body).span,
                )
            })?;
        let crate::StableProducerId::Function(canonical_function_identity) = &canonical_producer
        else {
            return Err(CompileError::new(
                ErrorKind::InternalError(
                    "callable body has a non-function canonical producer".into(),
                ),
                self.body_rir_ref().get(body).span,
            ));
        };
        let canonical_function_identity = (**canonical_function_identity).clone();
        let mut ctx = AnalysisContext {
            producer: body,
            canonical_producer,
            canonical_producer_arguments,
            canonical_function_identity,
            current_file_id: self.body_rir_ref().get(body).span.file_id,
            locals: AHashMap::new(),
            params: &param_vec,
            param_index: &param_index,
            next_slot: 0,
            loop_depth: 0,
            checked_depth: 0,
            loop_break_stack: Vec::new(),
            used_locals: AHashSet::new(),
            return_type,
            scope_stack: Vec::new(),
            moved_scope_stack: Vec::new(),
            comptime_type_scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: AHashMap::new(),
            warnings: Vec::new(),
            allow_unused_variables: allow_unused_variable,
            local_string_table: AHashMap::new(),
            local_strings: Vec::new(),
            local_atoms: Vec::new(),
            comptime_type_vars,
            comptime_value_vars,
            referenced_functions: AHashSet::new(),
            referenced_methods: AHashSet::new(),
            byref_arg_root: None,
            call_loaned_roots: Vec::new(),
            in_loop_move_recheck: false,
            iter_borrows: Vec::new(),
            expected_type: None,
            infer_ctx,
            accessor_trailing_yield: None,
            accessor_call_insts: AHashMap::new(),
            expression_loans: Vec::new(),
            inline_resolved_types: Vec::new(),
            place_aliases: AHashMap::new(),
            try_operand: false,
        };

        // Accessor body shape (ADR-0062 phase 1): the body block must end in
        // a single trailing `yield`, and only that instruction may be a
        // `yield` — every other non-diverging exit is E0254, enforced by the
        // per-instruction `yield`/`return`/`?` analysis against the trailing
        // reference recorded here.
        if is_accessor {
            // Signature legality is a declaration invariant. The whole-RIR
            // host freezes checked declarations before body analysis, and the
            // provider host obtains this method's facts through its successful
            // durable signature query. Re-checking 6.6:3-6.6:5 here would add
            // a third producer without admitting any new body-analysis path.
            ctx.accessor_trailing_yield = Some(super::declarations::accessor_trailing_yield(
                self.body_rir_ref(),
                body,
            )?);
        }

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types. A
        // `str`-returning function (ADR-0043 Phase 3, RUE-324) supplies `str` as
        // the expected type so an implicit-return string literal (the block's
        // tail expression) materializes as a static-backed first-class `str`.
        // Inner `let`s clear `expected_type` for their own initializers (they
        // `take()` it), so this only reaches the tail value.
        if self.is_str_like(return_type) {
            ctx.expected_type = Some(return_type);
        }
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;
        ctx.expected_type = None;

        // Linear parameters: the callee owns its pass-by-value parameters and
        // drops them at exit unless moved out (RUE-61), so a by-value
        // parameter carrying a linear value must be consumed by the body on
        // every path — exactly like a linear local (RUE-176). Inout/borrow
        // parameters stay owned by the caller and comptime parameters are
        // substituted away; destructors are exempt (see the doc comment).
        if !is_destructor {
            for p in &param_vec {
                if p.mode != RirParamMode::Normal || p.is_comptime {
                    continue;
                }
                if !self.type_requires_consumption(p.ty) {
                    continue;
                }
                let state = self.moved_state(&ctx, &p.name);
                if !state.is_some_and(|s| s.full_move_on_all_paths) {
                    // Element-wise consumption of a linear array parameter
                    // (RUE-186) satisfies the obligation like a whole move.
                    match self.check_array_elementwise_consumption(
                        p.ty,
                        state,
                        p.name,
                        self.body_rir_ref().get(body).span,
                    )? {
                        ElementwiseConsumption::Complete => continue,
                        ElementwiseConsumption::NotElementwise => {}
                    }
                    let name = self.body_interner().resolve(&p.name);
                    let err = linear_not_consumed_error(
                        name,
                        self.body_rir_ref().get(body).span,
                        state.and_then(|s| s.full_move),
                    )
                    .with_note(format!(
                        "parameter '{name}' is passed by value, so this function owns it \
                         and must consume it (pass it on, return it, or destructure it)"
                    ));
                    return Err(self.attach_infectious_linear_note(err, p.ty));
                }
            }
        }

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::NEVER {
            // An accessor result cannot be the function's tail value: the
            // borrowed place is scoped to its full expression (ADR-0062).
            {
                let tail = self.rir_block_tail_expr(body);
                self.reject_accessor_result_escape(
                    tail,
                    super::analysis::AccessorEscapeSite::Return,
                    self.body_rir_ref().get(tail).span,
                    &ctx,
                )?;
            }
            // Two-types model (ADR-0043, RUE-386): a `str`-returning function's
            // implicit-return (tail) value must be a first-class `str`. A buffer
            // (`StrBuf`/`Str(N)`) or a borrowed `str` view escaping here dangles
            // once its backing storage is dropped. (Explicit `return x;` is
            // checked in `analyze_return`.)
            if self.is_str_struct(return_type) {
                let tail = self.rir_block_tail_expr(body);
                self.reject_non_first_class_str(
                    tail,
                    body_result.ty,
                    FirstClassStrSite::Return,
                    self.body_rir_ref().get(tail).span,
                    &ctx,
                )?;
            }
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(body_result.air_ref)),
                ty: return_type,
                span: self.body_rir_ref().get(body).span,
            });
        }

        // AIR emission can select a nominal type through paths which already
        // carry a resolved `Type` (match patterns, inferred expressions, and
        // comptime type values) without calling the textual type resolver.
        // Observe those types while the current body owner is still installed;
        // this walks produced AIR values, never RIR, and retains no AIR.
        if self
            .storage
            .body_declaration_type_observer()
            .as_ref()
            .is_some_and(|observer| observer.4 == crate::DeclarationTypeDependencyKind::Body)
        {
            let mut observed_types =
                Vec::with_capacity(1 + param_vec.len() + air.instructions().len());
            observed_types.push(return_type);
            observed_types.extend(param_vec.iter().map(|param| param.ty));
            observed_types.extend(air.instructions().iter().map(|inst| inst.ty));
            self.storage
                .body_analysis_work_mut()
                .body_dependency_air_instructions_observed += air.instructions().len();
            for ty in observed_types {
                self.storage.record_resolved_declaration_type(ty);
            }
        }

        if self.storage.body_analysis_error_recovery()
            && self.storage.body_analysis_first_recovered_error().is_some()
        {
            return Err(self
                .storage
                .body_analysis_first_recovered_error()
                .expect("error was checked"));
        }

        let air_emission_validation_ns =
            u64::try_from(air_emission_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.storage
            .record_expression_analysis_breakdown(ExpressionAnalysisBreakdown {
                setup_ns: expression_setup_ns,
                inference_precompute_ns: inference_breakdown.precompute_ns,
                inference_precompute_structural_ns: inference_breakdown.precompute_structural_ns,
                inference_precompute_eval_provider_ns: inference_breakdown
                    .precompute_eval_provider_ns,
                precompute_work: inference_breakdown.precompute_work,
                constraint_generation_ns: inference_breakdown.constraint_generation_ns,
                unification_resolution_ns: inference_breakdown.unification_resolution_ns,
                air_emission_validation_ns,
            });

        Ok((
            air,
            ctx.next_slot,
            num_param_slots,
            param_modes,
            ctx.warnings,
            ctx.local_strings,
            ctx.local_atoms,
            ctx.referenced_functions,
            ctx.referenced_methods,
        ))
    }
}
