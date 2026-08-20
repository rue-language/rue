//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use super::ordinary_engine::{OrdinaryBodyAnalysisHost, OrdinaryBodyEngine};
use ahash::{AHashMap, AHashSet};
use std::convert::Infallible;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::{FileId, Span};

/// Frame-displacement budget a single object's slots must address: `i32::MAX`
/// bytes, matching the codegen frame-offset (disp32) addressing range (spec
/// C.4:3, RUE-561). This is the derivation basis for [`MAX_TYPE_SLOTS`], not a
/// limit that is checked on its own: nothing compares a byte total against it.
pub(crate) const MAX_TYPE_SIZE_BYTES: u64 = i32::MAX as u64;
/// Maximum ABI slot count for a single object: [`MAX_TYPE_SIZE_BYTES`] divided
/// by the 8-byte slot width, i.e. 268,435,455 slots. This — not a byte total —
/// is the limit E0906 enforces and names, because a layout spends one slot per
/// scalar, struct field, and array element whatever the element's own width
/// (spec C.4:2, RUE-1272). A `[i8; N]` therefore stops at this many *elements*,
/// well under `MAX_TYPE_SIZE_BYTES` bytes. Types past it are rejected with
/// E0906 rather than wrapping the slot arithmetic, per the graceful-failure
/// policy in spec C.1:2.
pub(crate) const MAX_TYPE_SLOTS: u64 = MAX_TYPE_SIZE_BYTES / 8;
use crate::sema::ConstValue;
use crate::types::{ArrayLen, Type, TypeKind};

/// The narrow semantic surface consumed by the canonical type-syntax
/// evaluator.  This deliberately owns neither a declaration epoch nor a body
/// analysis state: callers provide the host that owns the current facts.
pub(super) trait TypeSyntaxHost {
    fn type_syntax_symbol(&mut self, name: &str) -> Spur;
    fn type_syntax_module_binding(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>;
    fn type_syntax_module_display_name(
        &self,
        module: crate::types::ModuleId,
    ) -> std::sync::Arc<str>;
    fn type_syntax_accessing_domain(
        &self,
        authority: TypeRootAuthority,
    ) -> crate::SemanticVisibilityDomain;
    fn type_syntax_named_type(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
        kind: TypeSyntaxNamedKind,
    ) -> CompileResult<Option<crate::SemanticTypeFact<Type, FileId>>>;
    fn type_syntax_make_str(&mut self, span: Span) -> CompileResult<Type>;
    fn type_syntax_make_array(
        &mut self,
        element: Type,
        length: u64,
        span: Span,
    ) -> CompileResult<Type>;
    fn type_syntax_make_ptr_const(&mut self, pointee: Type, span: Span) -> CompileResult<Type>;
    fn type_syntax_make_ptr_mut(&mut self, pointee: Type, span: Span) -> CompileResult<Type>;
    fn type_syntax_make_slice(
        &mut self,
        syntax: &str,
        element: Type,
        span: Span,
    ) -> CompileResult<Type>;
    fn type_syntax_make_fixed_str(&mut self, capacity: u64, span: Span) -> CompileResult<Type>;
    fn type_syntax_record_builtin_call(&mut self);
    fn type_syntax_constructor(
        &mut self,
        authority: TypeRootAuthority,
        module: Option<crate::types::ModuleId>,
        name: Spur,
    ) -> CompileResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>>;
    fn type_syntax_reduce_constructor(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, Type)],
        value_arguments: &[(Spur, ConstValue)],
        span: Span,
    ) -> CompileResult<Option<ConstValue>>;
    fn type_syntax_value_const(&self, file: FileId, name: Spur) -> Option<super::info::ConstInfo>;
    fn type_syntax_recover_const(
        &mut self,
        file: FileId,
        name: Spur,
    ) -> CompileResult<Option<ConstValue>>;
    fn type_syntax_record_named_const_dependency(&mut self, file: FileId, name: String);
    fn type_syntax_out_of_scope_const_hint(&self, name: Spur, exclude: FileId) -> String;
    fn type_syntax_dependencies(
        &self,
        ty: Type,
    ) -> Vec<(FileId, String, super::DeclarationTypeDependencyTargetKind)>;
    fn type_syntax_flush_dependency(
        &mut self,
        file: FileId,
        name: String,
        kind: super::DeclarationTypeDependencyTargetKind,
    );
}

#[derive(Clone, Copy)]
pub(super) enum TypeSyntaxNamedKind {
    Struct,
    Enum,
    Alias,
}

type ObservedTypeDependency = (FileId, String, super::DeclarationTypeDependencyTargetKind);

pub(super) struct TypeSyntaxProvider<'host, 'c, H: TypeSyntaxHost> {
    host: &'host mut H,
    span: Span,
    root_authority: TypeRootAuthority,
    resolution_context: SemaTypeResolutionContext,
    type_substitutions: Option<&'c AHashMap<Spur, Type>>,
    value_substitutions: Option<&'c AHashMap<Spur, ConstValue>>,
    // Preserve observation order for the host while using a lazy membership
    // index once a resolution grows beyond the allocation-free small case.
    observed_type_dependencies: Vec<ObservedTypeDependency>,
    observed_type_dependency_index: Option<AHashSet<ObservedTypeDependency>>,
}

/// The lexical root a type-syntax resolution walks from.
///
/// A source name only means something relative to the file whose declarations
/// and imports are in scope: two sibling modules may each declare `Cell`, and
/// the referencing file decides which one an unqualified mention selects
/// (RUE-497). This type therefore has exactly one form — a known file — and no
/// scope-free "global"/speculative construction; RUE-1126 deleted the last one.
///
/// Do not reintroduce a variant that stands for "no scope". Synthetic and
/// builtin types stay reachable without one: builtin struct/enum name
/// resolution is consulted from inside the scoped path, so a builtin lookup
/// never needs to bypass the file namespace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct TypeRootAuthority {
    file: FileId,
}

impl TypeRootAuthority {
    /// Anchor resolution at `file`. This is the only constructor: a caller
    /// cannot ask for a lookup that has no lexical scope.
    pub(super) fn in_file(file: FileId) -> Self {
        Self { file }
    }

    /// The file whose source namespace this resolution reads.
    pub(super) fn file(self) -> FileId {
        self.file
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SemaTypeResolutionContext {
    Type,
    ArrayLength,
}

type SemaProviderResult<T> = crate::SemanticProviderResult<T, Infallible, CompileError>;

fn provider_failure<T>(result: CompileResult<T>) -> SemaProviderResult<T> {
    result.map_err(crate::SemanticProviderError::Failure)
}

impl<H: TypeSyntaxHost> crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>
    for TypeSyntaxProvider<'_, '_, H>
{
    type Abort = Infallible;
    type Failure = CompileError;

    fn root_module_binding(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(
            self.host
                .type_syntax_module_binding(self.root_authority, None, symbol),
        )
    }

    fn module_binding(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        let name = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_module_binding(
            self.root_authority,
            Some(*module),
            name,
        ))
    }

    fn module_display_name(&self, module: &crate::types::ModuleId) -> std::sync::Arc<str> {
        self.host.type_syntax_module_display_name(*module)
    }

    fn accessing_domain(&self, _scope: &FileId) -> crate::SemanticVisibilityDomain {
        self.host.type_syntax_accessing_domain(self.root_authority)
    }
}

impl<H: TypeSyntaxHost>
    crate::SemanticTypeSyntaxProvider<
        FileId,
        crate::types::ModuleId,
        FileId,
        Spur,
        Spur,
        Type,
        ConstValue,
    > for TypeSyntaxProvider<'_, '_, H>
{
    fn substituted_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<Type>> {
        let Some(type_substitutions) = self.type_substitutions else {
            return Ok(None);
        };
        let symbol = self.host.type_syntax_symbol(name);
        Ok(type_substitutions.get(&symbol).copied())
    }

    fn primitive_type(&mut self, name: &str) -> SemaProviderResult<Option<Type>> {
        Ok(Type::from_primitive_name(name))
    }

    fn builtin_type(&mut self, _scope: &FileId, name: &str) -> SemaProviderResult<Option<Type>> {
        if name == "str" {
            provider_failure(self.host.type_syntax_make_str(self.span).map(Some))
        } else {
            Ok(None)
        }
    }

    fn root_struct_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            None,
            symbol,
            TypeSyntaxNamedKind::Struct,
        ))
    }

    fn root_enum_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            None,
            symbol,
            TypeSyntaxNamedKind::Enum,
        ))
    }

    fn root_type_alias(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            None,
            symbol,
            TypeSyntaxNamedKind::Alias,
        ))
    }

    fn module_struct_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            Some(*module),
            symbol,
            TypeSyntaxNamedKind::Struct,
        ))
    }

    fn module_enum_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            Some(*module),
            symbol,
            TypeSyntaxNamedKind::Enum,
        ))
    }

    fn module_type_alias(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_named_type(
            self.root_authority,
            Some(*module),
            symbol,
            TypeSyntaxNamedKind::Alias,
        ))
    }

    fn observe_selected_named_type(
        &mut self,
        name: &str,
        kind: crate::SemanticTypeFactKind,
        fact: &crate::SemanticTypeFact<Type, FileId>,
    ) -> SemaProviderResult<()> {
        match kind {
            crate::SemanticTypeFactKind::Struct | crate::SemanticTypeFactKind::Enum => {
                self.observe_materialized_type_fact(fact.value);
            }
            crate::SemanticTypeFactKind::Constant => {
                self.observe_type_dependency(
                    fact.site,
                    name.to_string(),
                    super::DeclarationTypeDependencyTargetKind::ValueConst,
                );
            }
            crate::SemanticTypeFactKind::Function => {}
        }
        Ok(())
    }

    fn observe_materialized_type(&mut self, ty: &Type) -> SemaProviderResult<()> {
        self.observe_materialized_type_fact(*ty);
        Ok(())
    }

    fn allows_qualified_paths(&self, _scope: &FileId) -> bool {
        true
    }

    fn allows_qualified_comptime_call_head(
        &self,
        _scope: &FileId,
        expectation: crate::SemanticComptimeCallExpectation,
    ) -> bool {
        !(self.resolution_context == SemaTypeResolutionContext::ArrayLength
            && expectation == crate::SemanticComptimeCallExpectation::Value)
    }

    fn resolve_array_length(
        &mut self,
        scope: &FileId,
        length: crate::SemanticValueSyntax<'_>,
    ) -> SemaProviderResult<Option<u64>> {
        let length = match length {
            crate::SemanticValueSyntax::Integer(value) => {
                let value = u64::try_from(value).map_err(|_| {
                    crate::SemanticProviderError::Failure(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!("array length must be non-negative, got {value}"),
                        },
                        self.span,
                    ))
                })?;
                ArrayLen::Literal(value)
            }
            crate::SemanticValueSyntax::Name(name) => ArrayLen::Named(name.to_owned()),
        };
        provider_failure(self.resolve_array_length_fact(*scope, &length)).map(Some)
    }

    fn array_length_from_value(
        &mut self,
        _scope: &FileId,
        value: &ConstValue,
    ) -> SemaProviderResult<Option<u64>> {
        match value {
            ConstValue::Integer(value) => u64::try_from(*value).map(Some).map_err(|_| {
                crate::SemanticProviderError::Failure(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!("array length must be non-negative, got {value}"),
                    },
                    self.span,
                ))
            }),
            _ => provider_failure(Err(CompileError::new(
                ErrorKind::InvalidArrayLength {
                    reason: "array length must be an integer".to_string(),
                },
                self.span,
            ))),
        }
    }

    fn array_type(&mut self, element: Type, length: Option<u64>) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_array(
            element,
            length.expect("concrete type resolution always resolves array lengths"),
            self.span,
        ))
    }

    fn ptr_const_type(&mut self, pointee: Type) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_ptr_const(pointee, self.span))
    }

    fn ptr_mut_type(&mut self, pointee: Type) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_ptr_mut(pointee, self.span))
    }

    fn slice_type(
        &mut self,
        _scope: &FileId,
        syntax: &str,
        element: Type,
    ) -> SemaProviderResult<Type> {
        provider_failure(self.host.type_syntax_make_slice(syntax, element, self.span))
    }

    fn builtin_type_call(
        &mut self,
        scope: &FileId,
        name: &str,
        arguments: &[crate::SemanticValueSyntax<'_>],
    ) -> SemaProviderResult<Option<Type>> {
        if name == "Str" {
            let capacity = match arguments {
                [crate::SemanticValueSyntax::Integer(value)] => {
                    u64::try_from(*value).map_err(|_| {
                        crate::SemanticProviderError::Failure(CompileError::new(
                            ErrorKind::InvalidArrayLength {
                                reason: format!("array length must be non-negative, got {value}"),
                            },
                            self.span,
                        ))
                    })?
                }
                [crate::SemanticValueSyntax::Name(argument)] => {
                    provider_failure(self.resolve_array_length_fact(
                        *scope,
                        &ArrayLen::Named((*argument).to_owned()),
                    ))?
                }
                _ => {
                    return provider_failure(Err(CompileError::new(
                        ErrorKind::UnknownType(format!("{name}(...)")),
                        self.span,
                    )));
                }
            };
            self.host.type_syntax_record_builtin_call();
            provider_failure(
                self.host
                    .type_syntax_make_fixed_str(capacity, self.span)
                    .map(Some),
            )
        } else {
            Ok(None)
        }
    }

    fn root_constructor(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(
            self.host
                .type_syntax_constructor(self.root_authority, None, symbol),
        )
    }

    fn module_constructor(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let symbol = self.host.type_syntax_symbol(name);
        provider_failure(self.host.type_syntax_constructor(
            self.root_authority,
            Some(*module),
            symbol,
        ))
    }

    fn resolve_value_argument(
        &mut self,
        scope: &FileId,
        constructor: &str,
        _head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        _parameter_index: usize,
        _type_arguments: &[(Spur, Type)],
        _value_arguments: &[(Spur, ConstValue)],
        syntax: crate::SemanticValueSyntax<'_>,
    ) -> SemaProviderResult<ConstValue> {
        if let crate::SemanticValueSyntax::Integer(value) = syntax {
            return Ok(ConstValue::Integer(value));
        }
        match self.resolve_value_argument_fact(*scope, constructor, syntax) {
            Ok(value) => Ok(value),
            Err(error) if self.resolution_context == SemaTypeResolutionContext::ArrayLength => {
                let crate::SemanticValueSyntax::Name(name) = syntax else {
                    return provider_failure(Err(error));
                };
                provider_failure(
                    self.resolve_array_length_fact(*scope, &ArrayLen::Named(name.to_owned()))
                        .map(|value| ConstValue::Integer(value as i128)),
                )
            }
            Err(error) => provider_failure(Err(error)),
        }
    }

    fn reduce_comptime_call(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, Type)],
        value_arguments: &[(Spur, ConstValue)],
    ) -> SemaProviderResult<Option<crate::SemanticComptimeCallResult<Type, ConstValue>>> {
        provider_failure(
            self.host
                .type_syntax_reduce_constructor(head, type_arguments, value_arguments, self.span)
                .map(|result| match (head.returns_type, result) {
                    (_, None) => None,
                    (true, Some(ConstValue::Type(ty))) => {
                        Some(crate::SemanticComptimeCallResult::Type(ty))
                    }
                    (false, Some(value)) => Some(crate::SemanticComptimeCallResult::Value(value)),
                    (true, Some(_)) => None,
                }),
        )
    }
}

impl<'s, 'c, H: TypeSyntaxHost> TypeSyntaxProvider<'s, 'c, H> {
    pub(super) fn new(
        host: &'s mut H,
        span: Span,
        root_authority: TypeRootAuthority,
        resolution_context: SemaTypeResolutionContext,
        type_substitutions: Option<&'c AHashMap<Spur, Type>>,
        value_substitutions: Option<&'c AHashMap<Spur, ConstValue>>,
    ) -> Self {
        Self {
            host,
            span,
            root_authority,
            resolution_context,
            type_substitutions,
            value_substitutions,
            observed_type_dependencies: Vec::new(),
            observed_type_dependency_index: None,
        }
    }

    pub(super) fn resolve_array_length_fact(
        &mut self,
        scope: FileId,
        length: &ArrayLen,
    ) -> CompileResult<u64> {
        let previous_context = self.resolution_context;
        self.resolution_context = SemaTypeResolutionContext::ArrayLength;
        let result = self.resolve_array_length_fact_inner(scope, length);
        self.resolution_context = previous_context;
        result
    }

    fn resolve_array_length_fact_inner(
        &mut self,
        _scope: FileId,
        length: &ArrayLen,
    ) -> CompileResult<u64> {
        let ArrayLen::Named(name) = length else {
            let ArrayLen::Literal(value) = length else {
                unreachable!()
            };
            return Ok(*value);
        };
        let symbol = self.host.type_syntax_symbol(name);
        let root_file = self.root_authority.file();
        let value = if let Some(value) = self
            .value_substitutions
            .and_then(|substitutions| substitutions.get(&symbol))
        {
            *value
        } else if let Some(info) = self.host.type_syntax_value_const(root_file, symbol) {
            self.host
                .type_syntax_record_named_const_dependency(info.span.file_id, name.to_owned());
            info.value
        } else if let Some(value) = self.host.type_syntax_recover_const(root_file, symbol)? {
            self.host
                .type_syntax_record_named_const_dependency(root_file, name.to_owned());
            value
        } else {
            let hint = self
                .host
                .type_syntax_out_of_scope_const_hint(symbol, root_file);
            return Err(self.invalid_array_length(format!(
                "'{name}' is not a compile-time constant; array lengths must be an integer literal, a `const`, or a `comptime` value parameter{hint}"
            )));
        };

        match value.as_int_value() {
            Some(value) if value >= 0 => u64::try_from(value).map_err(|_| {
                self.invalid_array_length(format!("array length '{name}' ({value}) is too large"))
            }),
            Some(value) => {
                Err(self
                    .invalid_array_length(format!("array length '{name}' is negative ({value})")))
            }
            None => {
                Err(self.invalid_array_length(format!("array length '{name}' is not an integer")))
            }
        }
    }

    fn invalid_array_length(&self, reason: String) -> CompileError {
        CompileError::new(ErrorKind::InvalidArrayLength { reason }, self.span)
    }

    fn observe_materialized_type_fact(&mut self, ty: Type) {
        for (file, name, kind) in self.host.type_syntax_dependencies(ty) {
            self.observe_type_dependency(file, name, kind);
        }
    }

    pub(super) fn flush_observed_type_dependencies(&mut self) {
        if let Some(index) = &mut self.observed_type_dependency_index {
            index.clear();
        }
        for (file, name, kind) in std::mem::take(&mut self.observed_type_dependencies) {
            self.host.type_syntax_flush_dependency(file, name, kind);
        }
    }

    fn observe_type_dependency(
        &mut self,
        file: FileId,
        name: String,
        kind: super::DeclarationTypeDependencyTargetKind,
    ) {
        const LINEAR_ADMISSION_LIMIT: usize = 8;

        let dependency = (file, name, kind);
        if let Some(index) = &mut self.observed_type_dependency_index {
            if index.insert(dependency.clone()) {
                self.observed_type_dependencies.push(dependency);
            }
            return;
        }
        if !self.observed_type_dependencies.contains(&dependency) {
            self.observed_type_dependencies.push(dependency);
            if self.observed_type_dependencies.len() == LINEAR_ADMISSION_LIMIT {
                self.observed_type_dependency_index =
                    Some(self.observed_type_dependencies.iter().cloned().collect());
            }
        }
    }

    fn resolve_value_argument_fact(
        &mut self,
        _scope: FileId,
        constructor: &str,
        syntax: crate::SemanticValueSyntax<'_>,
    ) -> CompileResult<ConstValue> {
        let crate::SemanticValueSyntax::Name(text) = syntax else {
            let crate::SemanticValueSyntax::Integer(value) = syntax else {
                unreachable!()
            };
            return Ok(ConstValue::Integer(value));
        };
        if text == "true" {
            return Ok(ConstValue::Bool(true));
        }
        if text == "false" {
            return Ok(ConstValue::Bool(false));
        }
        let symbol = self.host.type_syntax_symbol(text);
        if let Some(value_substitutions) = self.value_substitutions
            && let Some(value) = value_substitutions.get(&symbol)
        {
            return Ok(*value);
        }
        if let Some(type_substitutions) = self.type_substitutions
            && let Some(ty) = type_substitutions.get(&symbol)
        {
            return Ok(ConstValue::Type(*ty));
        }
        if let Some(info) = self
            .host
            .type_syntax_value_const(self.root_authority.file(), symbol)
        {
            return Ok(info.value);
        }
        Err(CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "argument '{}' of type constructor '{}' must be a compile-time known value (an integer or bool literal, a comptime parameter, or a constant)",
                    text, constructor
                ),
            },
            self.span,
        ))
    }
}

fn module_path_compile_error(
    failure: crate::SemanticModulePathFailure<FileId>,
    span: Span,
) -> CompileError {
    match failure {
        crate::SemanticModulePathFailure::Empty => {
            CompileError::new(ErrorKind::UnknownType(String::new()), span)
        }
        crate::SemanticModulePathFailure::UnknownRoot { name } => {
            CompileError::new(ErrorKind::UnknownType(name.to_string()), span)
        }
        crate::SemanticModulePathFailure::UnknownMember { module, member, .. } => {
            CompileError::new(
                ErrorKind::UnknownModuleMember {
                    module_name: module.to_string(),
                    member_name: member.to_string(),
                },
                span,
            )
        }
        crate::SemanticModulePathFailure::PrivateMember {
            member,
            defining_file,
            ..
        } => private_qualified_item_error("constant", &member, &defining_file, span),
    }
}

pub(super) fn module_path_resolution_compile_error(
    failure: crate::SemanticResolutionError<
        Infallible,
        CompileError,
        crate::SemanticModulePathFailure<FileId>,
    >,
    span: Span,
) -> CompileError {
    match failure {
        crate::SemanticResolutionError::ProviderAbort(error) => match error {},
        crate::SemanticResolutionError::ProviderFailure(error) => error,
        crate::SemanticResolutionError::Semantic(failure) => {
            module_path_compile_error(failure, span)
        }
        crate::SemanticResolutionError::ComptimeCallTypeArgument { error, .. } => {
            module_path_resolution_compile_error(*error, span)
        }
    }
}

fn private_qualified_item_error(
    item_kind: &str,
    member: &str,
    defining_file: &str,
    span: Span,
) -> CompileError {
    CompileError::new(
        ErrorKind::PrivateUnqualifiedAccess(Box::new(
            rue_error::PrivateUnqualifiedAccessData {
                item_kind: item_kind.to_string(),
                name: member.to_string(),
                defining_file: defining_file.to_string(),
            },
        )),
        span,
    )
    .with_help(format!(
        "`{member}` is not marked `pub`; private items are only visible within their defining directory"
    ))
}

pub(super) fn semantic_type_syntax_compile_error(
    interner: &lasso::ThreadedRodeo,
    failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
    span: Span,
) -> CompileError {
    use crate::SemanticResolutionError as E;
    use crate::SemanticTypeSyntaxFailure as F;

    match failure {
        E::ProviderAbort(error) => match error {},
        E::ProviderFailure(error) => error,
        E::Semantic(F::Path(failure)) => module_path_compile_error(failure, span),
        E::Semantic(F::UnknownType { syntax }) => {
            CompileError::new(ErrorKind::UnknownType(syntax.to_string()), span)
        }
        E::Semantic(F::UnknownConstructor {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Type,
        }) => CompileError::new(ErrorKind::UnknownType(format!("{constructor}(...)")), span),
        E::Semantic(F::UnknownConstructor {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Value,
        }) => CompileError::new(
            ErrorKind::InvalidArrayLength {
                reason: format!(
                    "'{constructor}' is not a function; array lengths must be an integer literal, a `const`, a `comptime` value parameter, or a call to a comptime function"
                ),
            },
            span,
        ),
        E::Semantic(F::UnknownModuleMember { module, member, .. }) => CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: module.to_string(),
                member_name: member.to_string(),
            },
            span,
        ),
        E::Semantic(F::PrivateItem {
            kind,
            name,
            defining_file,
            ..
        }) => private_qualified_item_error(kind.diagnostic_name(), &name, &defining_file, span),
        E::Semantic(F::AmbiguousItem { name, .. }) => CompileError::new(
            ErrorKind::InternalError(format!(
                "type resolution produced an ambiguous item for '{}'",
                name
            )),
            span,
        ),
        E::Semantic(F::NotTypeConstructor { constructor, .. }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "'{}' is not a type: only a function returning `type` (a type constructor) can be applied as a type here",
                    constructor
                ),
            },
            span,
        ),
        E::Semantic(F::TypeWhereValueExpected { constructor, .. }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "'{}' returns `type` and cannot be used where a compile-time value is required",
                    constructor
                ),
            },
            span,
        ),
        E::Semantic(F::InvalidConstructorArity {
            constructor,
            expected,
            found,
            expectation: crate::SemanticComptimeCallExpectation::Type,
            ..
        })
        | E::Semantic(F::RuntimeConstructorParameter {
            constructor,
            expected,
            found,
            expectation: crate::SemanticComptimeCallExpectation::Type,
            ..
        }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "type constructor '{}' expects {} comptime type argument(s), but {} were provided",
                    constructor, expected, found
                ),
            },
            span,
        ),
        E::Semantic(F::InvalidConstructorArity {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Value,
            ..
        })
        | E::Semantic(F::RuntimeConstructorParameter {
            constructor,
            expectation: crate::SemanticComptimeCallExpectation::Value,
            ..
        }) => CompileError::new(
            ErrorKind::InvalidArrayLength {
                reason: format!(
                    "array length call '{constructor}(...)' is not a compile-time constant; its callee must be a value-returning function whose parameters are all `comptime`"
                ),
            },
            span,
        ),
        E::Semantic(F::ValueWhereTypeExpected {
            constructor,
            argument,
            parameter,
            ..
        }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "argument '{}' of type constructor '{}' must be a type (this parameter is `comptime {}: type`)",
                    argument,
                    constructor,
                    interner.resolve(&parameter)
                ),
            },
            span,
        ),
        E::ComptimeCallTypeArgument { error, .. } => {
            semantic_type_syntax_compile_error(interner, *error, span)
        }
        E::Semantic(F::ConstructorDidNotReduce { constructor, .. }) => CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "the type constructor '{}' did not reduce to a concrete type at compile time",
                    constructor
                ),
            },
            span,
        ),
    }
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Is `ty` the synthetic `str` struct (ADR-0043 Phase 3, RUE-324)? Detected
    /// by the struct name being exactly `str`. Used to route string literals and
    /// slice-style `.len()`/index operations through the fat-pointer paths while
    /// keeping `str` first-class (exempt from the slice second-class rule).
    pub(crate) fn is_str_struct(&self, ty: Type) -> bool {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            &*self.body_type_pool().struct_def(struct_id).name == "str"
        } else {
            false
        }
    }

    /// Is `ty` a fixed-capacity string `Str(N)` (ADR-0043 Phase 5, RUE-326)?
    /// Detected by the struct name matching `Str(<digits>)`, mirroring the
    /// name-keyed detection used for `str` and slices.
    pub(crate) fn is_str_fixed_struct(&self, ty: Type) -> bool {
        self.str_fixed_capacity(ty).is_some()
    }

    /// If `ty` is a fixed-capacity string `Str(N)`, return its capacity `N`
    /// (ADR-0043 Phase 5, RUE-326); otherwise `None`. The capacity is parsed
    /// back out of the canonical struct name `Str(<N>)`.
    pub(crate) fn str_fixed_capacity(&self, ty: Type) -> Option<u64> {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let name = &self.body_type_pool().struct_def(struct_id).name;
            crate::types::fixed_string_capacity(name)
        } else {
            None
        }
    }

    /// Is `ty` `str`-like — either the `str` slice view or a fixed-capacity
    /// `Str(N)` (ADR-0043 Phases 3/5)? Both share the 2-word `{ptr, len}`
    /// representation and the UTF-8 byte-string convention, so string-literal
    /// materialization, `.len()`, packed byte-indexing, and by-value passing all
    /// treat them identically. The capacity-fits legality rule is the only place
    /// `Str(N)` diverges from `str`.
    pub(crate) fn is_str_like(&self, ty: Type) -> bool {
        self.is_str_struct(ty) || self.is_str_fixed_struct(ty)
    }
}

impl<H: OrdinaryBodyAnalysisHost> OrdinaryBodyEngine<'_, H> {
    /// Reject a type whose layout exceeds the implementation's maximum object
    /// size — [`MAX_TYPE_SLOTS`] ABI slots (Appendix C practical limit,
    /// RUE-561) — returning the slot count on success. Call this wherever a
    /// value of `ty` is MATERIALIZED — a local or temporary slot allocation, a
    /// by-value parameter, `@size_of` / `@align_of` — so the saturating
    /// fallback in [`Self::abi_slot_count`] is never observable.
    pub(crate) fn require_layout_slots(&self, ty: Type, span: Span) -> CompileResult<u32> {
        match self.checked_abi_slot_count(ty) {
            Some(slots) => Ok(slots as u32),
            None => Err(CompileError::new(
                ErrorKind::TypeTooLarge {
                    type_name: ty.safe_name_with_pool(Some(self.body_type_pool())),
                    max_slots: MAX_TYPE_SLOTS,
                },
                span,
            )),
        }
    }

    /// Reserve a cumulative local or parameter frame region without allowing
    /// individually valid layouts to overflow the function-wide displacement
    /// budget (RUE-780).
    pub(crate) fn reserve_frame_slots(
        &self,
        current: &mut u32,
        additional: u32,
        span: Span,
    ) -> CompileResult<u32> {
        let start = *current;
        *current =
            crate::layout::checked_function_frame_slots(start, additional).ok_or_else(|| {
                CompileError::new(
                    ErrorKind::FunctionFrameTooLarge {
                        max_bytes: crate::layout::MAX_FUNCTION_FRAME_BYTES,
                    },
                    span,
                )
            })?;
        Ok(start)
    }

    /// Checked companion to [`Self::abi_slot_count`]: `None` when the type's
    /// layout overflows or exceeds [`MAX_TYPE_SLOTS`] (RUE-561). Computed in
    /// u64 with checked arithmetic so large array lengths cannot truncate to
    /// zero slots or overflow the slot-count multiplication.
    pub(crate) fn checked_abi_slot_count(&self, ty: Type) -> Option<u64> {
        let slots = match ty.kind() {
            TypeKind::Array(array_type_id) => {
                let (element_type, length) = self.body_type_pool().array_def(array_type_id);
                let element_slots = self.checked_abi_slot_count(element_type)?;
                element_slots.checked_mul(length)?
            }
            TypeKind::Struct(struct_id) => {
                let struct_def = self.body_type_pool().struct_def(struct_id);
                let mut total = 0u64;
                for f in &struct_def.fields {
                    total = total.checked_add(self.checked_abi_slot_count(f.ty)?)?;
                }
                total
            }
            TypeKind::Enum(enum_id) => {
                let enum_def = self.body_type_pool().enum_def(enum_id);
                let mut max_payload = 0u64;
                for i in 0..enum_def.variant_count() {
                    let mut variant_slots = 0u64;
                    for &vty in enum_def.variant_payload(i) {
                        variant_slots =
                            variant_slots.checked_add(self.checked_abi_slot_count(vty)?)?;
                    }
                    max_payload = max_payload.max(variant_slots);
                }
                1 + max_payload
            }
            // Every other kind is 0 or 1 slots; delegate.
            _ => u64::from(self.abi_slot_count(ty)),
        };
        (slots <= MAX_TYPE_SLOTS).then_some(slots)
    }

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot,
    /// structs use 1 slot per field, arrays use 1 slot per element.
    /// Zero-sized types (unit, never, empty structs, zero-length arrays) use 0 slots.
    ///
    /// Layout arithmetic SATURATES (no overflow panic, no silent u32
    /// truncation — RUE-561); an oversized type is rejected with E0906 at
    /// every materialization site via [`Self::require_layout_slots`], so the
    /// saturated value is never used for real allocation.
    pub(crate) fn abi_slot_count(&self, ty: Type) -> u32 {
        self.body_type_pool().provisional_abi_slot_count(ty)
    }
}
