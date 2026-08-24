//! Host-generic canonical compile-time evaluator.
//!
//! This module owns every recursive RIR edge for the local comptime path. Hosts
//! provide semantic facts and side effects through named hooks; they never walk
//! child instructions or invoke another evaluator.

use ahash::AHashMap;
use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::{InstData, InstRef, RepeatCount, Rir};
use rue_span::{FileId, Span};

use super::comptime_eval::{ComptimeEnv, comptime_panic_err, const_int_fits};
use super::info::FunctionCallInfo;
use crate::integer_semantics::CheckedIntegerResult;
use crate::intern_pool::TypeInternPool;
use crate::specialize::MAX_COMPTIME_CALL_DEPTH;
use crate::types::{ArrayLen, ArrayTypeId, StructField, Type, TypeKind};

/// Value algebra consumed by the canonical dispatcher.  The surrounding
/// semantic type system remains local in this migration checkpoint; hosts may
/// provide any value representation that can carry these four comptime forms.
pub(crate) trait ComptimeValue: Clone {
    fn integer(value: i128) -> Self;
    fn boolean(value: bool) -> Self;
    fn unit() -> Self;
    fn type_value(value: Type) -> Self;
    fn as_integer(&self) -> Option<i128>;
    fn as_boolean(&self) -> Option<bool>;
    fn as_type(&self) -> Option<Type>;
}

/// A declaration-level constant fact in the engine's value domain.  The
/// adapter supplies only the metadata needed for dependency/privacy handling
/// and a value when that declaration is representable by the current domain.
#[derive(Debug, Clone)]
pub(crate) struct ComptimeConstInfo<V> {
    pub(crate) is_pub: bool,
    pub(crate) span: Span,
    pub(crate) value: Option<V>,
}

#[cfg(test)]
mod value_domain_tests {
    use super::*;
    use crate::sema::{AnonMethodSig, anon_structs};

    #[derive(Clone, Debug, PartialEq)]
    enum FakeValue {
        Integer(i128),
        Boolean(bool),
        Unit,
        Type(Type),
    }

    impl ComptimeValue for FakeValue {
        fn integer(value: i128) -> Self {
            Self::Integer(value)
        }
        fn boolean(value: bool) -> Self {
            Self::Boolean(value)
        }
        fn unit() -> Self {
            Self::Unit
        }
        fn type_value(_value: Type) -> Self {
            Self::Type(_value)
        }
        fn as_integer(&self) -> Option<i128> {
            match self {
                Self::Integer(value) => Some(*value),
                _ => None,
            }
        }
        fn as_boolean(&self) -> Option<bool> {
            match self {
                Self::Boolean(value) => Some(*value),
                _ => None,
            }
        }
        fn as_type(&self) -> Option<Type> {
            match self {
                Self::Type(value) => Some(*value),
                _ => None,
            }
        }
    }

    struct FakeHost {
        rir: Rir,
        interner: lasso::ThreadedRodeo,
        pool: TypeInternPool,
    }

    impl ComptimeHost for FakeHost {
        type Value = FakeValue;
        fn program_rir(&self) -> &Rir {
            &self.rir
        }
        fn body_interner(&self) -> &lasso::ThreadedRodeo {
            &self.interner
        }
        fn body_type_pool(&self) -> &TypeInternPool {
            &self.pool
        }
        fn value_const(&self, _key: &(FileId, Spur)) -> Option<ComptimeConstInfo<Self::Value>> {
            None
        }
        fn match_pattern(
            &self,
            _pattern: &rue_rir::RirPatternView<'_>,
            _value: &Self::Value,
        ) -> Option<bool> {
            None
        }
        fn require_preview(
            &self,
            _feature: rue_error::PreviewFeature,
            _what: &str,
            _span: Span,
        ) -> CompileResult<()> {
            Ok(())
        }
        fn record_body_named_dependency(
            &mut self,
            _target: super::super::NamedConstDependencyTargetEvent,
        ) {
        }
        fn reduce_external_comptime_call(
            &mut self,
            _name: Spur,
            _types: &AHashMap<Spur, Type>,
            _values: &AHashMap<Spur, Self::Value>,
            _span: Span,
        ) -> Option<CompileResult<Option<Self::Value>>> {
            None
        }
        fn resolve_array_length(
            &mut self,
            _length: &ArrayLen,
            _span: Span,
            _values: Option<&AHashMap<Spur, Self::Value>>,
        ) -> CompileResult<u64> {
            Ok(0)
        }
        fn rir_type_named_symbol(&self, _syntax: rue_rir::RirTypeSyntaxRef) -> Option<Spur> {
            None
        }
        fn get_or_create_array_type(&mut self, _element: Type, _length: u64) -> ArrayTypeId {
            panic!("fake host does not construct array types")
        }
        fn extract_anon_method_sigs(
            &mut self,
            _methods: &rue_rir::RirAnonStructMethodsRange,
            _types: &AHashMap<Spur, Type>,
            _values: &AHashMap<Spur, Self::Value>,
        ) -> Vec<AnonMethodSig> {
            panic!("fake host does not construct anonymous methods")
        }
        fn find_method_own_comptime_type_param(
            &self,
            _methods: &rue_rir::RirAnonStructMethodsRange,
        ) -> Option<(Span, String)> {
            None
        }
        fn find_or_create_anon_struct(
            &mut self,
            _identity: anon_structs::IssuedAnonymousNominalKey,
            _fields: &[StructField],
            _sigs: &[AnonMethodSig],
            _captured: &AHashMap<Spur, Self::Value>,
        ) -> CompileResult<(Type, bool)> {
            panic!("fake host does not construct anonymous structs")
        }
        fn find_or_create_anon_enum(
            &mut self,
            _identity: anon_structs::IssuedAnonymousNominalKey,
            _names: &[String],
            _payloads: &[Vec<Type>],
        ) -> CompileResult<Type> {
            panic!("fake host does not construct anonymous enums")
        }
        fn has_method(&self, _key: (crate::types::StructId, Spur)) -> bool {
            false
        }
        fn check_unqualified_visibility(
            &self,
            _item_kind: &str,
            _name: &str,
            _defining_file_id: FileId,
            _is_pub: bool,
            _span: Span,
        ) -> CompileResult<()> {
            Ok(())
        }
        fn check_require_droppable(&mut self, _ty: Type, _span: Span) -> CompileResult<()> {
            Ok(())
        }
        fn check_trivially_droppable(&mut self, _ty: Type, _span: Span) -> CompileResult<()> {
            Ok(())
        }
        fn const_expr_type(
            &self,
            _env: &ComptimeEnv<'_, Self::Value>,
            _inst_ref: InstRef,
        ) -> Option<Type> {
            None
        }
        fn finish_arith(
            &self,
            result: CheckedIntegerResult,
            _ty: Option<Type>,
            _op: &str,
            _span: Span,
        ) -> CompileResult<Option<Self::Value>> {
            Ok(result.checked().map(FakeValue::integer))
        }
        fn resolve_named_type_value(
            &mut self,
            _name: Spur,
            _span: Span,
        ) -> CompileResult<Option<Type>> {
            Ok(None)
        }
        fn resolve_comptime_type_path(
            &mut self,
            _file: FileId,
            _segments: &[Spur],
            _span: Span,
        ) -> CompileResult<Option<Self::Value>> {
            Ok(None)
        }
        fn resolve_module_comptime_callable(
            &mut self,
            _file_id: FileId,
            _segments: &[Spur],
            _method: Spur,
            _span: Span,
        ) -> CompileResult<Option<Spur>> {
            Ok(None)
        }
        fn admit_comptime_call(
            &mut self,
            _name: Spur,
            _arg_count: usize,
            _arg_modes: &[ComptimeArgMode],
            _env: &mut ComptimeEnv<'_, Self::Value>,
            _name_is_resolved_key: bool,
        ) -> CompileResult<Option<ComptimeCallAdmission>> {
            Ok(None)
        }
        fn bind_comptime_call(
            &self,
            _admission: &ComptimeCallAdmission,
            _values: &[Self::Value],
            _span: Span,
        ) -> CompileResult<Option<(AHashMap<Spur, Type>, AHashMap<Spur, Self::Value>)>> {
            Ok(None)
        }
        fn prepare_local_comptime_call(
            &mut self,
            _admission: ComptimeCallAdmission,
            _types: AHashMap<Spur, Type>,
            _values: AHashMap<Spur, Self::Value>,
            _span: Span,
        ) -> CompileResult<Option<PreparedComptimeCall<Self::Value>>> {
            Ok(None)
        }
        fn finish_comptime_call(
            &mut self,
            _plan: &PreparedComptimeCall<Self::Value>,
            result: CompileResult<Option<Self::Value>>,
        ) -> CompileResult<Option<Self::Value>> {
            result
        }
        fn label_ctor_instantiation_site(error: CompileError, _call_span: Span) -> CompileError {
            error
        }
        fn canonical_function_producer(
            &self,
            _name: Spur,
            _types: &AHashMap<Spur, Type>,
            _values: &AHashMap<Spur, Self::Value>,
        ) -> Result<anon_structs::IssuedStableProducerId, crate::SemanticBodyExportFailure>
        {
            panic!("fake host does not issue producers")
        }
        fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
            &mut self,
            _syntax: rue_rir::RirTypeSyntaxRef,
            _types: &AHashMap<Spur, Type>,
            _values: &AHashMap<Spur, Self::Value>,
            _span: Span,
        ) -> Option<Type> {
            None
        }
        fn register_anon_struct_methods_for_comptime_with_subst(
            &mut self,
            _struct_id: crate::types::StructId,
            _struct_type: Type,
            _methods: &rue_rir::RirAnonStructMethodsRange,
            _types: &AHashMap<Spur, Type>,
            _values: &AHashMap<Spur, Self::Value>,
        ) -> Option<()> {
            None
        }
        fn set_anon_struct_type_subst(
            &mut self,
            _struct_id: crate::types::StructId,
            _subst: AHashMap<Spur, Type>,
        ) {
        }
    }

    #[test]
    fn non_local_value_domain_runs_the_real_arithmetic_dispatcher() {
        let mut editor = rue_rir::RirEditor::new();
        let lhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(40),
            span: Span::new(0, 0),
        });
        let rhs = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(2),
            span: Span::new(0, 0),
        });
        let add = editor.add_inst(rue_rir::Inst {
            data: InstData::Add { lhs, rhs },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            rir: editor.finish(),
            interner: lasso::ThreadedRodeo::new(),
            pool: TypeInternPool::new(),
        };
        let mut env = ComptimeEnv::<FakeValue>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(add, &mut env)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Integer(42));
    }

    #[test]
    fn non_local_value_domain_runs_the_real_branch_dispatcher() {
        let mut editor = rue_rir::RirEditor::new();
        let condition = editor.add_inst(rue_rir::Inst {
            data: InstData::BoolConst(true),
            span: Span::new(0, 0),
        });
        let then_value = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(7),
            span: Span::new(0, 0),
        });
        let else_value = editor.add_inst(rue_rir::Inst {
            data: InstData::IntConst(9),
            span: Span::new(0, 0),
        });
        let branch = editor.add_inst(rue_rir::Inst {
            data: InstData::Branch {
                cond: condition,
                then_block: then_value,
                else_block: Some(else_value),
            },
            span: Span::new(0, 0),
        });
        let mut host = FakeHost {
            rir: editor.finish(),
            interner: lasso::ThreadedRodeo::new(),
            pool: TypeInternPool::new(),
        };
        let mut env = ComptimeEnv::<FakeValue>::new();
        let value = ComptimeEngine::new(&mut host)
            .evaluate(branch, &mut env)
            .unwrap()
            .unwrap();
        assert_eq!(value, FakeValue::Integer(7));
    }
}

#[derive(Debug)]
pub(crate) struct PreparedComptimeCall<V> {
    pub(crate) name: Spur,
    pub(crate) body: InstRef,
    pub(crate) file: FileId,
    pub(crate) span: Span,
    pub(crate) function_span: Span,
    pub(crate) callee_types: AHashMap<Spur, Type>,
    pub(crate) callee_values: AHashMap<Spur, V>,
}

pub(crate) type ComptimeArgMode = (rue_rir::RirArgMode, Span);

#[derive(Debug, Clone, Copy)]
pub(crate) struct ComptimeCallAdmission {
    pub(crate) name: Spur,
    pub(crate) function: FunctionCallInfo,
}

/// Semantic host boundary for the canonical dispatcher. No method accepts an
/// instruction callback or a child RIR reference for evaluation.
pub(crate) trait ComptimeHost {
    type Value: ComptimeValue;
    fn program_rir(&self) -> &Rir;
    fn body_interner(&self) -> &lasso::ThreadedRodeo;
    fn body_type_pool(&self) -> &TypeInternPool;
    fn value_const(&self, key: &(FileId, Spur)) -> Option<ComptimeConstInfo<Self::Value>>;
    fn match_pattern(
        &self,
        pattern: &rue_rir::RirPatternView<'_>,
        value: &Self::Value,
    ) -> Option<bool>;
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()>;
    fn record_body_named_dependency(&mut self, target: super::NamedConstDependencyTargetEvent);
    fn reduce_external_comptime_call(
        &mut self,
        name: Spur,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, Self::Value>,
        span: Span,
    ) -> Option<CompileResult<Option<Self::Value>>>;
    fn resolve_array_length(
        &mut self,
        length: &ArrayLen,
        span: Span,
        values: Option<&AHashMap<Spur, Self::Value>>,
    ) -> CompileResult<u64>;
    fn rir_type_named_symbol(&self, syntax: rue_rir::RirTypeSyntaxRef) -> Option<Spur>;
    fn get_or_create_array_type(&mut self, element: Type, length: u64) -> ArrayTypeId;
    fn extract_anon_method_sigs(
        &mut self,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, Self::Value>,
    ) -> Vec<super::AnonMethodSig>;
    fn find_method_own_comptime_type_param(
        &self,
        methods: &rue_rir::RirAnonStructMethodsRange,
    ) -> Option<(Span, String)>;
    fn find_or_create_anon_struct(
        &mut self,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
        fields: &[StructField],
        sigs: &[super::AnonMethodSig],
        captured: &AHashMap<Spur, Self::Value>,
    ) -> CompileResult<(Type, bool)>;
    fn find_or_create_anon_enum(
        &mut self,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
        names: &[String],
        payloads: &[Vec<Type>],
    ) -> CompileResult<Type>;
    fn has_method(&self, key: (crate::types::StructId, Spur)) -> bool;
    fn check_unqualified_visibility(
        &self,
        item_kind: &str,
        name: &str,
        defining_file_id: FileId,
        is_pub: bool,
        span: Span,
    ) -> CompileResult<()>;
    fn check_require_droppable(&mut self, ty: Type, span: Span) -> CompileResult<()>;
    fn check_trivially_droppable(&mut self, ty: Type, span: Span) -> CompileResult<()>;
    fn const_expr_type(
        &self,
        env: &ComptimeEnv<'_, Self::Value>,
        inst_ref: InstRef,
    ) -> Option<Type>;
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Type>,
        op: &str,
        span: Span,
    ) -> CompileResult<Option<Self::Value>>;
    fn resolve_named_type_value(&mut self, _name: Spur, span: Span) -> CompileResult<Option<Type>>;
    fn resolve_comptime_type_path(
        &mut self,
        file: FileId,
        segments: &[Spur],
        span: Span,
    ) -> CompileResult<Option<Self::Value>>;
    fn resolve_module_comptime_callable(
        &mut self,
        file_id: FileId,
        segments: &[Spur],
        method: Spur,
        span: Span,
    ) -> CompileResult<Option<Spur>>;
    fn admit_comptime_call(
        &mut self,
        name: Spur,
        arg_count: usize,
        arg_modes: &[ComptimeArgMode],
        env: &mut ComptimeEnv<'_, Self::Value>,
        name_is_resolved_key: bool,
    ) -> CompileResult<Option<ComptimeCallAdmission>>;
    fn bind_comptime_call(
        &self,
        admission: &ComptimeCallAdmission,
        values: &[Self::Value],
        span: Span,
    ) -> CompileResult<Option<(AHashMap<Spur, Type>, AHashMap<Spur, Self::Value>)>>;
    fn prepare_local_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission,
        types: AHashMap<Spur, Type>,
        values: AHashMap<Spur, Self::Value>,
        span: Span,
    ) -> CompileResult<Option<PreparedComptimeCall<Self::Value>>>;
    fn finish_comptime_call(
        &mut self,
        plan: &PreparedComptimeCall<Self::Value>,
        result: CompileResult<Option<Self::Value>>,
    ) -> CompileResult<Option<Self::Value>>;
    fn label_ctor_instantiation_site(error: CompileError, call_span: Span) -> CompileError;
    fn canonical_function_producer(
        &self,
        name: Spur,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, Self::Value>,
    ) -> Result<super::anon_structs::IssuedStableProducerId, crate::SemanticBodyExportFailure>;
    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, Self::Value>,
        span: Span,
    ) -> Option<Type>;
    fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        struct_id: crate::types::StructId,
        struct_type: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, Self::Value>,
    ) -> Option<()>;
    fn set_anon_struct_type_subst(
        &mut self,
        struct_id: crate::types::StructId,
        subst: AHashMap<Spur, Type>,
    );
}

pub(crate) struct ComptimeEngine<'e, H: ComptimeHost> {
    host: &'e mut H,
    call_depth: usize,
}

impl<'e, H: ComptimeHost> ComptimeEngine<'e, H> {
    pub(crate) fn new(host: &'e mut H) -> Self {
        Self {
            host,
            call_depth: 0,
        }
    }

    pub(crate) fn evaluate(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv<'_, H::Value>,
    ) -> CompileResult<Option<H::Value>> {
        self.eval(inst_ref, env)
    }

    /// Evaluate a named call through a child call. The body host receives
    /// only the semantically named call operation; recursive expression edges
    /// stay in this engine.
    fn evaluate_call(
        &mut self,
        name: Spur,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv<'_, H::Value>,
        span: Span,
    ) -> CompileResult<Option<H::Value>> {
        let args = self.host.program_rir().call_args(args).to_vec();
        let arg_modes: Vec<ComptimeArgMode> = args
            .iter()
            .map(|arg| (arg.mode, self.host.program_rir().get(arg.value).span))
            .collect();
        let Some(admission) =
            self.host
                .admit_comptime_call(name, args.len(), &arg_modes, env, false)?
        else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(args.len());
        for arg in &args {
            let Some(value) = self.eval(arg.value, env)? else {
                return Ok(None);
            };
            values.push(value);
        }
        let Some((callee_types, callee_values)) =
            self.host.bind_comptime_call(&admission, &values, span)?
        else {
            return Ok(None);
        };
        if let Some(result) = self.host.reduce_external_comptime_call(
            admission.name,
            &callee_types,
            &callee_values,
            span,
        ) {
            return result;
        }
        let Some(plan) =
            self.host
                .prepare_local_comptime_call(admission, callee_types, callee_values, span)?
        else {
            return Ok(None);
        };
        self.enter_call(plan, span)
    }

    fn enter_call(
        &mut self,
        plan: PreparedComptimeCall<H::Value>,
        call_span: Span,
    ) -> CompileResult<Option<H::Value>> {
        self.run_prepared(plan, call_span)
    }

    pub(crate) fn evaluate_prepared_root(
        &mut self,
        plan: PreparedComptimeCall<H::Value>,
    ) -> CompileResult<Option<H::Value>> {
        let span = plan.span;
        // Direct type-constructor reductions are calls in the language model;
        // their root consumes the first call-depth slot. Expression roots,
        // entered through the expression adapter, are not calls and do not enter
        // this method.
        self.run_prepared(plan, span)
    }

    fn run_prepared(
        &mut self,
        plan: PreparedComptimeCall<H::Value>,
        call_span: Span,
    ) -> CompileResult<Option<H::Value>> {
        if self.call_depth >= MAX_COMPTIME_CALL_DEPTH {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "specialization of '{}' exceeded the maximum nesting depth ({}); \
                         is a comptime-recursive function missing a compile-time-known \
                         base case, or a generic function recursively instantiating \
                         itself with new types?",
                        self.host.body_interner().resolve(&plan.name),
                        MAX_COMPTIME_CALL_DEPTH
                    ),
                },
                plan.function_span,
            ));
        }
        let canonical_identity = self
            .host
            .canonical_function_producer(plan.name, &plan.callee_types, &plan.callee_values)
            .map_err(|failure| {
                CompileError::new(
                    ErrorKind::InternalError(format!(
                        "failed to issue canonical comptime producer: {failure:?}"
                    )),
                    plan.span,
                )
            })?;
        let body = plan.body;
        let mut child_env = ComptimeEnv::with_subst(&plan.callee_types, &plan.callee_values);
        child_env.producer = Some(body);
        child_env.canonical_identity = Some(canonical_identity);
        child_env.defining_file = Some(plan.file);
        self.call_depth += 1;
        let result = self.eval(body, &mut child_env);
        self.call_depth -= 1;
        let result = result.map_err(|error| H::label_ctor_instantiation_site(error, call_span));
        self.host.finish_comptime_call(&plan, result)
    }

    fn evaluate_method_call(
        &mut self,
        receiver: InstRef,
        method: Spur,
        args: &rue_rir::RirCallArgsRange,
        env: &mut ComptimeEnv<'_, H::Value>,
        span: Span,
    ) -> CompileResult<Option<H::Value>> {
        let args = self.host.program_rir().call_args(args).to_vec();
        let Some((file_id, segments)) = self.decode_module_path(receiver, env)? else {
            return Ok(None);
        };
        let Some(name) = self
            .host
            .resolve_module_comptime_callable(file_id, &segments, method, span)?
        else {
            return Ok(None);
        };
        let arg_modes: Vec<ComptimeArgMode> = args
            .iter()
            .map(|arg| (arg.mode, self.host.program_rir().get(arg.value).span))
            .collect();
        let Some(admission) =
            self.host
                .admit_comptime_call(name, args.len(), &arg_modes, env, true)?
        else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(args.len());
        for arg in &args {
            let Some(value) = self.eval(arg.value, env)? else {
                return Ok(None);
            };
            values.push(value);
        }
        let Some((callee_types, callee_values)) =
            self.host.bind_comptime_call(&admission, &values, span)?
        else {
            return Ok(None);
        };
        if let Some(result) = self.host.reduce_external_comptime_call(
            admission.name,
            &callee_types,
            &callee_values,
            span,
        ) {
            return result;
        }
        let Some(plan) =
            self.host
                .prepare_local_comptime_call(admission, callee_types, callee_values, span)?
        else {
            return Ok(None);
        };
        self.enter_call(plan, span)
    }

    /// Decode only the syntactic module path for a method call. Resolution of
    /// the path's declarations and visibility stays in the semantic host; the
    /// engine owns this RIR edge so hosts never need to inspect child
    /// instructions to discover a callable.
    fn decode_module_path(
        &self,
        receiver: InstRef,
        env: &ComptimeEnv<'_, H::Value>,
    ) -> CompileResult<Option<(FileId, Vec<Spur>)>> {
        let mut chain_rev = Vec::new();
        let mut cursor = receiver;
        let root = loop {
            match self.host.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break name,
                InstData::FieldGet { base, field } => {
                    chain_rev.push(field);
                    cursor = base;
                }
                _ => return Ok(None),
            }
        };
        if env.locals.contains_key(&root)
            || env
                .runtime_locals
                .is_some_and(|locals| locals.contains_key(&root))
            || env
                .runtime_binding_names
                .is_some_and(|names| names.contains(&root))
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return Ok(None);
        }
        let Some(file_id) = env.defining_file else {
            return Ok(None);
        };
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Ok(Some((file_id, segments)))
    }

    /// Decode a dotted type path before crossing the host boundary. The host
    /// receives only copied semantic path facts; it never needs to inspect the
    /// RIR spine or an evaluation environment to decide whether this is a
    /// module/type path.
    fn decode_type_path(
        &self,
        inst_ref: InstRef,
        env: &ComptimeEnv<'_, H::Value>,
    ) -> CompileResult<Option<(FileId, Vec<Spur>)>> {
        let mut chain_rev = Vec::new();
        let mut cursor = inst_ref;
        let root = loop {
            match self.host.program_rir().get(cursor).data {
                InstData::VarRef { name, .. } => break name,
                InstData::FieldGet { base, field } => {
                    chain_rev.push(field);
                    cursor = base;
                }
                _ => return Ok(None),
            }
        };
        if env.locals.contains_key(&root)
            || env
                .runtime_locals
                .is_some_and(|locals| locals.contains_key(&root))
            || env
                .runtime_binding_names
                .is_some_and(|names| names.contains(&root))
            || env
                .runtime_params
                .is_some_and(|(params, index)| index.get(params, root).is_some())
            || env.type_subst.contains_key(&root)
            || env.value_subst.contains_key(&root)
        {
            return Ok(None);
        }
        let Some(file_id) = env.defining_file else {
            return Ok(None);
        };
        chain_rev.reverse();
        let mut segments = Vec::with_capacity(chain_rev.len() + 1);
        segments.push(root);
        segments.extend(chain_rev);
        Ok(Some((file_id, segments)))
    }

    fn eval_int_operands(
        &mut self,
        lhs: InstRef,
        rhs: InstRef,
        env: &mut ComptimeEnv<'_, H::Value>,
    ) -> CompileResult<Option<(i128, i128)>> {
        let Some(l) = self.eval(lhs, env)?.and_then(|v| v.as_integer()) else {
            return Ok(None);
        };
        let Some(r) = self.eval(rhs, env)?.and_then(|v| v.as_integer()) else {
            return Ok(None);
        };
        Ok(Some((l, r)))
    }

    /// The single compile-time evaluation engine. See the module docs for the
    /// result encoding (`Ok(Some)` / `Ok(None)` / `Err`).
    fn eval(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv<'_, H::Value>,
    ) -> CompileResult<Option<H::Value>> {
        let inst = {
            let source = self.host.program_rir().get(inst_ref);
            rue_rir::Inst {
                data: source.data.clone(),
                span: source.span,
            }
        };
        let span = inst.span;
        match &inst.data {
            // Integer literals. The literal itself must fit its resolved type
            // (the inner expression of a comptime block never goes through
            // `analyze_literal`, so this is where `300` at type u8 is caught).
            InstData::IntConst(value) => {
                let v = *value as i128;
                if let Some(ty) = self.host.const_expr_type(env, inst_ref) {
                    if !const_int_fits(v, ty) {
                        return Err(CompileError::new(
                            ErrorKind::LiteralOutOfRange {
                                value: *value,
                                ty: ty.safe_name_with_pool(Some(self.host.body_type_pool())),
                            },
                            span,
                        ));
                    }
                }
                Ok(Some(H::Value::integer(v)))
            }

            // Float literals stop here for the same reason they stop in
            // `analyze_inst_dispatch` (ADR-0065, RUE-1069): there is no
            // `comptime_float` value in `ConstValue` yet. Naming the real
            // reason matters more here than elsewhere — falling through to
            // the generic "not knowable at compile time" would be actively
            // wrong about a literal, which is the most compile-time-knowable
            // thing there is. Delete this arm when Phase 4 lands.
            InstData::FloatConst { .. } => {
                self.host.require_preview(
                    rue_error::PreviewFeature::Floats,
                    "a floating-point literal",
                    span,
                )?;
                Err(CompileError::new(ErrorKind::FloatNotYetImplemented, span))
            }

            // Boolean literals
            InstData::BoolConst(value) => Ok(Some(H::Value::boolean(*value))),

            // Unit literal
            InstData::UnitConst => Ok(Some(H::Value::unit())),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                let ty = self.host.const_expr_type(env, inst_ref);
                if let Some(ty) = ty {
                    if ty.is_unsigned() {
                        return Err(CompileError::new(
                            ErrorKind::CannotNegate(
                                ty.safe_name_with_pool(Some(self.host.body_type_pool())),
                            ),
                            span,
                        ));
                    }
                }
                if let InstData::IntConst(magnitude) = &self.host.program_rir().get(*operand).data {
                    // The literal path uses mathematical magnitude semantics:
                    // unlike an ordinary runtime value, `128` must not first
                    // canonicalize to -128 before becoming `-128`.
                    let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                        || CheckedIntegerResult::from_raw((*magnitude as i128).checked_neg()),
                        |integer| integer.checked_neg_literal_report_i128(*magnitude as i128),
                    );
                    self.host.finish_arith(result, ty, "-", span)
                } else {
                    match self.eval(*operand, env)? {
                        Some(value) => {
                            let Some(n) = value.as_integer() else {
                                return Ok(None);
                            };
                            let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                                || CheckedIntegerResult::from_raw(n.checked_neg()),
                                |integer| integer.checked_neg_report_i128(n),
                            );
                            self.host.finish_arith(result, ty, "-", span)
                        }
                        // Can't negate a boolean, type, or unit
                        _ => Ok(None),
                    }
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.eval(*operand, env)? {
                    Some(value) => match value.as_boolean() {
                        Some(b) => Ok(Some(H::Value::boolean(!b))),
                        None => Ok(None),
                    },
                    // Can't logical-NOT an integer, type, or unit
                    _ => Ok(None),
                }
            }

            // Binary arithmetic operations, checked at the operand type
            InstData::Add { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.host.const_expr_type(env, inst_ref);
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || CheckedIntegerResult::from_raw(l.checked_add(r)),
                    |integer| integer.checked_add_report_i128(l, r),
                );
                self.host.finish_arith(result, ty, "+", span)
            }
            InstData::Sub { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.host.const_expr_type(env, inst_ref);
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || CheckedIntegerResult::from_raw(l.checked_sub(r)),
                    |integer| integer.checked_sub_report_i128(l, r),
                );
                self.host.finish_arith(result, ty, "-", span)
            }
            InstData::Mul { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.host.const_expr_type(env, inst_ref);
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || CheckedIntegerResult::from_raw(l.checked_mul(r)),
                    |integer| integer.checked_mul_report_i128(l, r),
                );
                self.host.finish_arith(result, ty, "*", span)
            }
            InstData::Div { lhs, rhs } | InstData::Mod { lhs, rhs } => {
                let is_div = matches!(&inst.data, InstData::Div { .. });
                let op = if is_div { "/" } else { "%" };
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                let ty = self.host.const_expr_type(env, inst_ref);
                if r == 0 {
                    let what = if is_div { "division" } else { "remainder" };
                    return match ty {
                        Some(_) => Err(comptime_panic_err(
                            format!("{} by zero (this operation would panic at runtime)", what),
                            span,
                        )),
                        // Untyped fallback: defer to the runtime check.
                        None => Ok(None),
                    };
                }
                // Untyped evaluation retains its historical i64 fallback;
                // typed MIN / -1 trapping is owned by the kernel report.
                if r == -1 && ty.is_none() && l == i128::from(i64::MIN) {
                    return Ok(None);
                }
                let result = ty.and_then(|ty| ty.integer_semantics()).map_or_else(
                    || {
                        CheckedIntegerResult::from_raw(if is_div {
                            l.checked_div(r)
                        } else {
                            l.checked_rem(r)
                        })
                    },
                    |integer| {
                        if is_div {
                            integer.checked_div_report_i128(l, r)
                        } else {
                            integer.checked_rem_report_i128(l, r)
                        }
                    },
                );
                self.host.finish_arith(result, ty, op, span)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let l = self.eval(*lhs, env)?;
                let r = self.eval(*rhs, env)?;
                match (l, r) {
                    (Some(a), Some(b)) => match (
                        a.as_integer(),
                        b.as_integer(),
                        a.as_boolean(),
                        b.as_boolean(),
                    ) {
                        (Some(a), Some(b), _, _) => Ok(Some(H::Value::boolean(a == b))),
                        (_, _, Some(a), Some(b)) => Ok(Some(H::Value::boolean(a == b))),
                        _ => Ok(None),
                    },
                    _ => Ok(None), // Mixed or non-constant operands
                }
            }
            InstData::Ne { lhs, rhs } => {
                let l = self.eval(*lhs, env)?;
                let r = self.eval(*rhs, env)?;
                match (l, r) {
                    (Some(a), Some(b)) => match (
                        a.as_integer(),
                        b.as_integer(),
                        a.as_boolean(),
                        b.as_boolean(),
                    ) {
                        (Some(a), Some(b), _, _) => Ok(Some(H::Value::boolean(a != b))),
                        (_, _, Some(a), Some(b)) => Ok(Some(H::Value::boolean(a != b))),
                        _ => Ok(None),
                    },
                    _ => Ok(None),
                }
            }
            InstData::Lt { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::boolean(l < r)))
            }
            InstData::Gt { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::boolean(l > r)))
            }
            InstData::Le { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::boolean(l <= r)))
            }
            InstData::Ge { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::boolean(l >= r)))
            }

            // Logical operations: short-circuit like the runtime, so a
            // non-constant (or would-panic) RHS is irrelevant when the LHS
            // already decides the result.
            InstData::And { lhs, rhs } => match self.eval(*lhs, env)? {
                Some(value) if value.as_boolean() == Some(false) => {
                    Ok(Some(H::Value::boolean(false)))
                }
                Some(value) if value.as_boolean() == Some(true) => match self.eval(*rhs, env)? {
                    Some(value) if value.as_boolean().is_some() => Ok(Some(value)),
                    _ => Ok(None),
                },
                _ => Ok(None),
            },
            InstData::Or { lhs, rhs } => match self.eval(*lhs, env)? {
                Some(value) if value.as_boolean() == Some(true) => {
                    Ok(Some(H::Value::boolean(true)))
                }
                Some(value) if value.as_boolean() == Some(false) => match self.eval(*rhs, env)? {
                    Some(value) if value.as_boolean().is_some() => Ok(Some(value)),
                    _ => Ok(None),
                },
                _ => Ok(None),
            },

            // Bitwise operations. For values in range of their type these are
            // closed (no overflow possible), so no range check is needed.
            InstData::BitAnd { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::integer(l & r)))
            }
            InstData::BitOr { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::integer(l | r)))
            }
            InstData::BitXor { lhs, rhs } => {
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                Ok(Some(H::Value::integer(l ^ r)))
            }

            // Shifts: the amount is masked modulo the bit width and the
            // result truncated to the operand width (spec 4.3a:10), exactly
            // matching the runtime semantics (RUE-29).
            InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {
                let is_shl = matches!(&inst.data, InstData::Shl { .. });
                let Some((l, r)) = self.eval_int_operands(*lhs, *rhs, env)? else {
                    return Ok(None);
                };
                match self.host.const_expr_type(env, inst_ref) {
                    Some(ty) => {
                        let integer = ty
                            .integer_semantics()
                            .expect("const_expr_type returned non-integer");
                        // Two's-complement AND masks negative amounts the same
                        // way the hardware masks the count register.
                        let v = integer.shift_i128(l, r, is_shl);
                        Ok(Some(H::Value::integer(v)))
                    }
                    None => {
                        // Without the operand type the width is unknown, so
                        // only fold amounts < 8 (safe for every width) and
                        // defer the rest to runtime.
                        if !(0..8).contains(&r) {
                            return Ok(None);
                        }
                        Ok(Some(H::Value::integer(if is_shl {
                            l << r
                        } else {
                            l >> r
                        })))
                    }
                }
            }

            // Bitwise NOT: truncated to the operand width (`~0` as u8 = 255).
            InstData::BitNot { operand } => {
                let Some(n) = self.eval(*operand, env)?.and_then(|v| v.as_integer()) else {
                    return Ok(None);
                };
                let v = match self.host.const_expr_type(env, inst_ref) {
                    Some(ty) => ty
                        .integer_semantics()
                        .expect("bitnot requires an integer type")
                        .bitnot_i128(n),
                    None => !n,
                };
                Ok(Some(H::Value::integer(v)))
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.eval(*expr, env),

            // Block: evaluate `let` statements into the environment, then the
            // tail expression. Loops, assignments and calls are not supported
            // and make the block non-evaluable.
            InstData::Block { instructions } => {
                let stmt_refs = self.host.program_rir().block_insts(instructions).to_vec();
                if stmt_refs.is_empty() {
                    return Ok(Some(H::Value::unit()));
                }
                // Bindings are scoped to the block.
                let saved_locals = env.locals.clone();
                let mut result = Some(H::Value::unit());
                for (i, stmt_ref) in stmt_refs.iter().copied().enumerate() {
                    let is_tail = i + 1 == stmt_refs.len();
                    let value = if let InstData::Alloc { name, init, .. } =
                        &self.host.program_rir().get(stmt_ref).data
                    {
                        let (name, init) = (*name, *init);
                        let Some(v) = self.eval(init, env)? else {
                            env.locals = saved_locals;
                            return Ok(None);
                        };
                        if let Some(name) = name {
                            env.locals.insert(name, v);
                        }
                        // A `let` statement itself evaluates to unit.
                        H::Value::unit()
                    } else {
                        let Some(v) = self.eval(stmt_ref, env)? else {
                            env.locals = saved_locals;
                            return Ok(None);
                        };
                        v
                    };
                    if is_tail {
                        result = Some(value);
                    }
                }
                env.locals = saved_locals;
                Ok(result)
            }

            // Comptime-known `if`: select the taken branch and reduce to its
            // value. This is what lets an `if` in a `-> type` body pick a
            // struct/enum branch at compile time (spec 4.14:17, RUE-262) — the
            // same branch selection ordinary comptime values already relied on
            // through the block/let path, now available as an expression. A
            // non-constant condition makes the whole `if` non-evaluable.
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                let (cond, then_block, else_block) = (*cond, *then_block, *else_block);
                match self.eval(cond, env)? {
                    Some(value) if value.as_boolean() == Some(true) => self.eval(then_block, env),
                    Some(value) if value.as_boolean() == Some(false) => match else_block {
                        Some(else_block) => self.eval(else_block, env),
                        // `if c { .. }` with no else yields unit when false.
                        None => Ok(Some(H::Value::unit())),
                    },
                    // Non-constant (or non-bool) condition: not evaluable.
                    _ => Ok(None),
                }
            }

            // Comptime-known `match`: evaluate the scrutinee, select the first
            // arm whose pattern matches, and reduce to that arm's body value
            // (spec 4.14:19, RUE-262). An enum-variant (`Path`) pattern isn't
            // representable as a `ConstValue`, and a non-constant scrutinee is
            // not decidable here — both make the `match` non-evaluable.
            InstData::Match { scrutinee, arms } => {
                let scrutinee = *scrutinee;
                let Some(scrut) = self.eval(scrutinee, env)? else {
                    return Ok(None);
                };
                let arms = self.host.program_rir().match_arms(arms).to_vec();
                for (pattern, body) in arms.iter() {
                    match self.host.match_pattern(pattern, &scrut) {
                        Some(true) => return self.eval(*body, env),
                        Some(false) => continue,
                        // Undecidable pattern (e.g. an enum-variant `Path`
                        // against a non-representable scrutinee): bail out.
                        None => return Ok(None),
                    }
                }
                // No arm matched. Exhaustiveness checking should make this
                // unreachable for a well-typed match; treat as non-evaluable.
                Ok(None)
            }

            // Anonymous struct type: evaluate to a comptime type value,
            // resolving field types through the type substitution.
            InstData::AnonStructType {
                fields,
                methods,
                anchor,
            } => {
                let field_decls = self.host.program_rir().anon_struct_fields(fields).to_vec();

                // Comptime `let` locals in scope participate in field-type
                // resolution (`let Inner = Mk(T); struct { x: Inner }`,
                // RUE-575), alongside the enclosing parameters.
                let (local_type_subst, local_value_subst) = env.substs_with_locals();

                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.host.body_interner().resolve(&name_sym).to_string();
                    // Field types resolve through both the type substitution
                    // (`comptime T: type`) and the value substitution
                    // (`comptime N: i32`, so an `[i32; N]` field gets a concrete
                    // length at each specialization; RUE-16).
                    let Some(field_ty) = self
                        .host
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            type_sym,
                            &local_type_subst,
                            &local_value_subst,
                            span,
                        )
                    else {
                        return Ok(None);
                    };
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.host.extract_anon_method_sigs(
                    methods,
                    &local_type_subst,
                    &local_value_subst,
                );

                let Some(producer) = env.canonical_identity.clone() else {
                    return Ok(None);
                };
                let (struct_ty, _is_new) = self.host.find_or_create_anon_struct(
                    crate::AnonymousNominalKey {
                        kind: crate::AnonymousNominalKind::Struct,
                        producer,
                        anchor: anchor.clone(),
                    },
                    &struct_fields,
                    &method_sigs,
                    &local_value_subst,
                )?;

                // Register methods if present and not yet registered for this
                // struct (it may have been created earlier without methods).
                if !self
                    .host
                    .program_rir()
                    .anon_struct_methods(methods)
                    .is_empty()
                {
                    // A method that declares its own `comptime T: type`
                    // parameter would need per-call monomorphization over that
                    // parameter, which is unsupported (RUE-284). Reject it at
                    // the method declaration so the enclosing `-> type`
                    // reduction cannot degrade into an unrelated E1200 at the
                    // instantiation site.
                    if let Some((method_span, method_name)) =
                        self.host.find_method_own_comptime_type_param(methods)
                    {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "method '{}' declares its own `comptime` type parameter, \
                                     which is not yet supported (a method cannot be \
                                     monomorphized over its own type parameter); \
                                     move the type parameter to the enclosing type \
                                     constructor instead",
                                    method_name
                                ),
                            },
                            method_span,
                        ));
                    }
                    let Some(struct_id) = struct_ty.as_struct() else {
                        return Ok(None);
                    };

                    let method_refs = self.host.program_rir().anon_struct_methods(methods);
                    let first_method_ref = method_refs.get(0).unwrap();
                    let first_method_inst = self.host.program_rir().get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration = !self.host.has_method((struct_id, *method_name));

                        if needs_registration
                            && self
                                .host
                                .register_anon_struct_methods_for_comptime_with_subst(
                                    struct_id,
                                    struct_ty,
                                    methods,
                                    &local_type_subst,
                                    &local_value_subst,
                                )
                                .is_none()
                        {
                            // Registration failure (e.g. duplicate method
                            // names) makes the type non-evaluable; the
                            // caller reports the comptime failure.
                            return Ok(None);
                        }

                        // Remember the enclosing type substitution (e.g.
                        // `T -> i32` for `Vec(i32)`) so it resolves inside every
                        // method *body*, not just the signatures registered
                        // above (RUE-313). Method bodies are analyzed later, in
                        // a separate pass that has no other way to recover the
                        // constructor's type parameters.
                        if needs_registration && !local_type_subst.is_empty() {
                            self.host
                                .set_anon_struct_type_subst(struct_id, local_type_subst.clone());
                        }
                    }
                }
                Ok(Some(H::Value::type_value(struct_ty)))
            }

            // Anonymous enum type: evaluate to a comptime type value, resolving
            // each variant's payload types through the type/value substitution.
            // The enum analog of the AnonStructType arm above — this is what
            // makes `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
            // monomorphize per instantiation (ADR-0038, RUE-6 phase 2).
            InstData::AnonEnumType {
                variants,
                payloads,
                anchor,
            } => {
                let variant_syms: Vec<lasso::Spur> = self
                    .host
                    .program_rir()
                    .anon_enum_variants(variants)
                    .to_vec();
                let payload_symbols: Vec<Vec<rue_rir::RirTypeSyntaxRef>> = self
                    .host
                    .program_rir()
                    .anon_enum_payloads(payloads, variants)
                    .map(|payload| payload.to_vec())
                    .collect();

                // Decode the self-describing payload region into per-variant
                // type-symbol lists (parallel to `variant_syms`), then resolve
                // each payload type through the substitutions.
                // Comptime `let` locals participate in payload-type
                // resolution, matching the struct arm (RUE-575).
                let (enum_type_subst, enum_value_subst) = env.substs_with_locals();

                let mut variant_names: Vec<String> = Vec::with_capacity(variant_syms.len());
                let mut variant_payloads: Vec<Vec<Type>> = Vec::with_capacity(variant_syms.len());
                for (&vsym, symbols) in variant_syms.iter().zip(payload_symbols) {
                    variant_names.push(self.host.body_interner().resolve(&vsym).to_string());
                    let mut tys: Vec<Type> = Vec::with_capacity(symbols.len());
                    for ty_sym in symbols {
                        let Some(ty) = self
                            .host
                            .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                                ty_sym,
                                &enum_type_subst,
                                &enum_value_subst,
                                span,
                            )
                        else {
                            return Ok(None);
                        };
                        tys.push(ty);
                    }
                    variant_payloads.push(tys);
                }

                let Some(producer) = env.canonical_identity.clone() else {
                    return Ok(None);
                };
                let enum_ty = self.host.find_or_create_anon_enum(
                    crate::AnonymousNominalKey {
                        kind: crate::AnonymousNominalKind::Enum,
                        producer,
                        anchor: anchor.clone(),
                    },
                    &variant_names,
                    &variant_payloads,
                )?;
                Ok(Some(H::Value::type_value(enum_ty)))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name = *type_name;
                // Type parameters in scope substitute first.
                if let Some(type_symbol) = self.host.rir_type_named_symbol(type_name) {
                    if let Some(&ty) = env.type_subst.get(&type_symbol) {
                        return Ok(Some(H::Value::type_value(ty)));
                    }
                    // A named type (primitive / struct / enum) resolves directly.
                    if let Some(ty) = self.host.resolve_named_type_value(type_symbol, span)? {
                        return Ok(Some(H::Value::type_value(ty)));
                    }
                }
                // A *composite* or *unit* type value — `[i32; 2]`, `()`,
                // `ptr const T` — is an equally-valid type argument (Appendix A
                // treats them as unambiguous type spellings; RUE-565). Its
                // TypeConst carries the composite spelling as the interned
                // `type_name`, so decode it through the full comptime type
                // resolver under the current substitutions (an inner element /
                // pointee naming an enclosing `comptime T` still resolves). An
                // unresolvable spelling stays non-evaluable (`None`).
                Ok(self
                    .host
                    .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        type_name,
                        env.type_subst,
                        &env.value_subst,
                        span,
                    )
                    .map(H::Value::type_value))
            }

            // An array-repeat expression `[T; N]` used as a comptime *type* value
            // (RUE-565). The surface form `[i32; 2]` in expression position parses
            // as an array-repeat literal whose element is a type value; when that
            // element reduces to a `ConstValue::Type`, the whole expression is the
            // array TYPE `[T; N]` — a legal type-constructor argument
            // (`Option([i32; 2])`). A repeat over a *runtime* element is a genuine
            // array value literal and is not comptime-foldable here (`None`).
            InstData::ArrayRepeat { value, count } => {
                let (value, count) = (*value, count.clone());
                let Some(value) = self.eval(value, env)? else {
                    return Ok(None);
                };
                let Some(elem_ty) = value.as_type() else {
                    return Ok(None);
                };
                let len = match count {
                    RepeatCount::Literal(n) => n,
                    RepeatCount::Named(sym) => {
                        let name = self.host.body_interner().resolve(&sym).to_string();
                        match self.host.resolve_array_length(
                            &ArrayLen::Named(name),
                            span,
                            Some(&env.value_subst),
                        ) {
                            Ok(n) => n,
                            Err(_) => return Ok(None),
                        }
                    }
                };
                let array_type_id = self.host.get_or_create_array_type(elem_ty, len);
                Ok(Some(H::Value::type_value(Type::new_array(array_type_id))))
            }

            // VarRef: comptime let-bindings, comptime parameters, file-level
            // constants, then type names.
            InstData::VarRef { name, .. } => {
                // 1. `let` bindings inside the comptime expression
                if let Some(v) = env.locals.get(name) {
                    return Ok(Some(v.clone()));
                }
                // 2. Runtime locals shadow comptime parameters and file-level
                //    constants: a reference that resolves to one is not
                //    compile-time evaluable (spec 4.14:6).
                if let Some(locals) = env.runtime_locals {
                    if locals.contains_key(name) {
                        return Ok(None);
                    }
                }
                if let Some(names) = env.runtime_binding_names
                    && names.contains(name)
                {
                    return Ok(None);
                }
                // 3. Comptime type parameters in scope
                if let Some(&ty) = env.type_subst.get(name) {
                    return Ok(Some(H::Value::type_value(ty)));
                }
                // 4. Comptime value parameters in scope
                if let Some(v) = env.value_subst.get(name) {
                    return Ok(Some(v.clone()));
                }
                // 5. Runtime parameters shadow file-level constants and type
                //    names. A comptime parameter with a concrete value was
                //    already handled by the substitution maps above.
                if let Some((params, param_index)) = env.runtime_params {
                    if param_index.get(params, *name).is_some() {
                        return Ok(None);
                    }
                }
                // 6. File-level constants: the value was evaluated once
                //    (and range-checked against the declared type) during
                //    declaration gathering — use it directly. Re-evaluating
                //    the initializer here would fail for forms only the
                //    declaration collector can resolve (module member
                //    access, RUE-160) and was exponential for const chains.
                //    Module-typed constants never appear in this table
                //    (module bindings are a distinct tagged resolution).
                //    Privacy applies here too (E0460, RUE-183): the table is
                //    global, so a const initializer in one directory could
                //    otherwise read a private constant from another. The
                //    VarRef's own span locates the referencing file;
                //    speculative callers (`try_evaluate_const*`) swallow the
                //    error and defer to runtime analysis, which re-checks.
                if let Some(info) = self.host.value_const(&(span.file_id, *name)) {
                    self.host.record_body_named_dependency(
                        super::NamedConstDependencyTargetEvent::ValueConst {
                            file: info.span.file_id.index(),
                            name: self.host.body_interner().resolve(name).to_string(),
                        },
                    );
                    self.host.check_unqualified_visibility(
                        "constant",
                        self.host.body_interner().resolve(name),
                        info.span.file_id,
                        info.is_pub,
                        span,
                    )?;
                    // String constants stay out of the comptime engine: no
                    // engine operation consumes them (no comptime string
                    // params or string arithmetic), so treat a reference as
                    // non-evaluable instead of leaking a value the arms
                    // below would mis-type (RUE-957). Use sites materialize
                    // string constants through the runtime path instead.
                    return Ok(info.value);
                }
                // 7. Type names used as values (e.g. `Point` in
                //    `fn make_type() -> type { Point }`)
                let resolved = self.host.resolve_named_type_value(*name, span)?;
                if let Some(ty) = resolved {
                    match ty.kind() {
                        TypeKind::Struct(id) => {
                            let def = self
                                .host
                                .body_type_pool()
                                .struct_metadata(id)
                                .expect("struct type must have declaration metadata");
                            self.host.record_body_named_dependency(
                                super::NamedConstDependencyTargetEvent::NamedType {
                                    file: def.file_id.index(),
                                    name: def.name.to_string(),
                                    kind: super::DeclarationTypeDependencyTargetKind::Struct,
                                },
                            );
                        }
                        TypeKind::Enum(id) => {
                            let def = self
                                .host
                                .body_type_pool()
                                .enum_metadata(id)
                                .expect("enum type must have declaration metadata");
                            self.host.record_body_named_dependency(
                                super::NamedConstDependencyTargetEvent::NamedType {
                                    file: def.file_id.index(),
                                    name: def.name.to_string(),
                                    kind: super::DeclarationTypeDependencyTargetKind::Enum,
                                },
                            );
                        }
                        _ => {}
                    }
                }
                Ok(resolved.map(H::Value::type_value))
            }

            // Call to a `-> type` function: reduce it to the resulting type
            // value when the callee is a type constructor and every argument
            // is compile-time known. This makes comptime type-function calls
            // compose in ANY position — a delegating return body
            // (`fn Alias() -> type { Point() }`), a nested argument
            // (`WrapA(WrapA(i32))`), and chains thereof (RUE-251).
            InstData::Call { name, args } => {
                let name = *name;
                self.evaluate_call(name, args, env, span)
            }

            // Module-member access (`m.CONST`) as an operand of a larger const
            // initializer. The value was pre-resolved from the module's file
            // (with privacy checks) before evaluation — see the
            // `const_module_members` field — since the engine has no file or
            // constant-collector context to resolve it here. A member absent
            // from the map may still be a member-access *type* path used as a
            // comptime type-constructor argument (`std.strbuf.StrBuf` in
            // `Result(std.strbuf.StrBuf, i32)`, RUE-948): resolve that chain to
            // its nominal type through the same walker the qualified
            // type-annotation position uses. A base that is neither a
            // pre-resolved member value nor a module type path (a runtime
            // value's field) stays non-evaluable, so the caller reports it
            // (RUE-267).
            InstData::FieldGet { .. } => {
                if let Some(value) = env.const_module_members.get(&inst_ref) {
                    return Ok(Some(value.clone()));
                }
                let Some((file, segments)) = self.decode_type_path(inst_ref, env)? else {
                    return Ok(None);
                };
                self.host.resolve_comptime_type_path(file, &segments, span)
            }

            // Type intrinsic in comptime position. `@require_droppable(T)` is the
            // owning-container well-formedness gate (RUE-388/RUE-646): std's
            // `ArrayBuf(T)` calls it in its `-> type` constructor body so that
            // instantiating the container with an element type it cannot yet
            // correctly own — one that is `linear` — is rejected at instantiation
            // time (E0499). Droppable-but-non-linear elements are accepted: the
            // container runs each live element's drop glue before freeing its
            // buffer (RUE-646). It reduces to unit so the surrounding block
            // body still yields the `struct { .. }` tail. `@size_of`/`@align_of`
            // are not comptime-foldable here and stay non-evaluable (spec
            // 4.14:29); `@int_max`/`@int_min` depend only on the type identity,
            // not layout, so they evaluate to their integer bound (RUE-694).
            InstData::TypeIntrinsic { name, type_arg } => {
                let (name, type_arg) = (*name, *type_arg);
                let gate = self.host.body_interner().resolve(&name);
                // Both well-formedness gates reduce to unit at comptime:
                // `@require_droppable` (instantiation-time, rejects `linear`) and
                // `@require_trivially_droppable` (read-time, rejects drop glue —
                // RUE-651). Any other type intrinsic (`@size_of`/`@align_of`) is
                // not comptime-foldable here.
                let is_droppable_gate = gate == "require_droppable";
                let is_trivial_gate = gate == "require_trivially_droppable";
                let is_int_bound = gate == "int_max" || gate == "int_min";
                if is_int_bound {
                    let is_max = gate == "int_max";
                    // A still-unresolved type parameter makes the intrinsic
                    // non-evaluable here; it folds at a concrete instantiation.
                    let Some(int_ty) = self
                        .host
                        .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                            type_arg,
                            env.type_subst,
                            &env.value_subst,
                            span,
                        )
                    else {
                        return Ok(None);
                    };
                    let bound = if is_max {
                        int_ty.int_max()
                    } else {
                        int_ty.int_min()
                    };
                    // A non-integer argument is diagnosed by runtime analysis
                    // (`analyze_type_intrinsic`, E0702); stay non-evaluable
                    // rather than duplicating the diagnostic.
                    return Ok(bound.map(H::Value::integer));
                }
                if !is_droppable_gate && !is_trivial_gate {
                    return Ok(None);
                }
                // Resolve the element type through the enclosing comptime
                // substitutions (`T -> Inner` for `ArrayBuf(Inner)`); a
                // still-unresolved type parameter makes the gate non-evaluable
                // (it will be re-checked at a concrete instantiation).
                let Some(elem_ty) = self
                    .host
                    .resolve_rir_type_for_comptime_with_subst_and_values_at_span(
                        type_arg,
                        env.type_subst,
                        &env.value_subst,
                        span,
                    )
                else {
                    return Ok(None);
                };
                if is_trivial_gate {
                    self.host.check_trivially_droppable(elem_ty, span)?;
                } else {
                    self.host.check_require_droppable(elem_ty, span)?;
                }
                Ok(Some(H::Value::unit()))
            }

            // Module-qualified comptime type-constructor call in value position,
            // e.g. `let O = b.Mk(T)` inside a `-> type` constructor body that is
            // being reduced (RUE-511). The receiver must be an unshadowed
            // `VarRef` naming a module binding of the *defining* file; membership
            // and visibility are validated before the call is reduced through the
            // same path unqualified calls take. Any other receiver (a runtime
            // value's method, a shadowed name) is a genuine runtime call and
            // stays non-evaluable.
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                let (receiver, method) = (*receiver, *method);
                self.evaluate_method_call(receiver, method, args, env, span)
            }

            // Everything else requires runtime evaluation
            _ => Ok(None),
        }
    }
}
