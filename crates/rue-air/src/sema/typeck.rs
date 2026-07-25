//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use std::collections::HashMap;
use std::convert::Infallible;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind, PreviewFeature};
use rue_span::{FileId, Span};

use super::context::AnalysisContext;
use super::{DeclarationPhase, Sema};

/// Maximum size of a single object in bytes: `i32::MAX`, matching the
/// codegen frame-offset (disp32) addressing range (Appendix C practical
/// limit, RUE-561). Types larger than this are rejected with E0906.
pub(crate) const MAX_TYPE_SIZE_BYTES: u64 = i32::MAX as u64;
/// [`MAX_TYPE_SIZE_BYTES`] expressed in 8-byte ABI slots.
pub(crate) const MAX_TYPE_SLOTS: u64 = MAX_TYPE_SIZE_BYTES / 8;
use super::info::FunctionInfo;
use crate::inference::InferType;
use crate::sema::ConstValue;
use crate::types::{
    ArrayLen, ArrayTypeId, StructId, Type, TypeKind, parse_array_type_syntax,
    parse_type_call_syntax,
};

struct SemaTypeSyntaxProvider<'s, 'a, 'c, D: DeclarationPhase> {
    sema: &'s mut Sema<'a, D>,
    span: Span,
    root_authority: SemaTypeRootAuthority,
    resolution_context: SemaTypeResolutionContext,
    type_substitutions: Option<&'c HashMap<Spur, Type>>,
    value_substitutions: Option<&'c HashMap<Spur, ConstValue>>,
    observed_type_dependencies: Vec<(FileId, String, super::DeclarationTypeDependencyTargetKind)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SemaTypeRootAuthority {
    KnownFile(FileId),
    GlobalSpeculative,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SemaTypeResolutionContext {
    Type,
    ArrayLength,
}

type SemaProviderResult<T> = crate::SemanticProviderResult<T, Infallible, CompileError>;

fn provider_failure<T>(result: CompileResult<T>) -> SemaProviderResult<T> {
    result.map_err(crate::SemanticProviderError::Failure)
}

impl<D: DeclarationPhase> crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>
    for SemaTypeSyntaxProvider<'_, '_, '_, D>
{
    type Abort = Infallible;
    type Failure = CompileError;

    fn root_module_binding(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        let SemaTypeRootAuthority::KnownFile(root_file) = self.root_authority else {
            return Ok(None);
        };
        let name = self.sema.interner.get_or_intern(name);
        self.sema.record_body_module_item_lookup(root_file, name);
        let binding = provider_failure(self.sema.resolve_module_binding_in_file(root_file, name))?;
        Ok(binding.and_then(|binding| self.module_binding_fact(binding)))
    }

    fn module_binding(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        if self.root_authority == SemaTypeRootAuthority::GlobalSpeculative {
            return Ok(None);
        }
        let file = self.sema.module_registry.get_def(*module).file_id;
        let name = self.sema.interner.get_or_intern(name);
        self.sema.record_body_module_item_lookup(file, name);
        let binding = provider_failure(self.sema.resolve_module_binding_in_file(file, name))?;
        Ok(binding.and_then(|binding| self.module_binding_fact(binding)))
    }

    fn module_display_name(&self, module: &crate::types::ModuleId) -> std::sync::Arc<str> {
        std::sync::Arc::from(
            self.sema
                .module_registry
                .get_def(*module)
                .import_path
                .as_str(),
        )
    }

    fn accessing_domain(&self, _scope: &FileId) -> crate::SemanticVisibilityDomain {
        match self.root_authority {
            SemaTypeRootAuthority::KnownFile(root_file) => {
                crate::SemanticVisibilityDomain::from_file_path(self.sema.get_file_path(root_file))
            }
            SemaTypeRootAuthority::GlobalSpeculative => {
                crate::SemanticVisibilityDomain::from_file_path(None)
            }
        }
    }
}

impl<D: DeclarationPhase>
    crate::SemanticTypeSyntaxProvider<
        FileId,
        crate::types::ModuleId,
        FileId,
        Spur,
        Spur,
        Type,
        ConstValue,
    > for SemaTypeSyntaxProvider<'_, '_, '_, D>
{
    fn substituted_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<Type>> {
        let Some(type_substitutions) = self.type_substitutions else {
            return Ok(None);
        };
        let symbol = self.sema.interner.get_or_intern(name);
        Ok(type_substitutions.get(&symbol).copied())
    }

    fn primitive_type(&mut self, name: &str) -> SemaProviderResult<Option<Type>> {
        Ok(Type::from_primitive_name(name))
    }

    fn builtin_type(&mut self, _scope: &FileId, name: &str) -> SemaProviderResult<Option<Type>> {
        if name == "str" && self.root_authority != SemaTypeRootAuthority::GlobalSpeculative {
            provider_failure(self.sema.get_or_create_str_struct(self.span).map(Some))
        } else {
            Ok(None)
        }
    }

    fn root_struct_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.sema.interner.get_or_intern(name);
        let (id, bypass_visibility) = match self.root_authority {
            SemaTypeRootAuthority::KnownFile(root_file) => {
                self.sema.record_body_module_item_lookup(root_file, symbol);
                (
                    self.sema
                        .structs_by_file_name
                        .get(&(root_file, symbol))
                        .copied()
                        .or_else(|| self.sema.resolve_builtin_struct_name(symbol)),
                    false,
                )
            }
            SemaTypeRootAuthority::GlobalSpeculative => (
                self.sema
                    .struct_id_for_name(symbol)
                    .or_else(|| self.sema.resolve_builtin_struct_name(symbol)),
                true,
            ),
        };
        let Some(id) = id else {
            return Ok(None);
        };
        let metadata = self
            .sema
            .type_pool
            .struct_metadata(id)
            .expect("type lookup names a registered struct identity");
        Ok(Some(self.type_fact(
            Type::new_struct(id),
            metadata.file_id,
            metadata.is_pub || bypass_visibility,
        )))
    }

    fn root_enum_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let symbol = self.sema.interner.get_or_intern(name);
        let (id, bypass_visibility) = match self.root_authority {
            SemaTypeRootAuthority::KnownFile(root_file) => {
                self.sema.record_body_module_item_lookup(root_file, symbol);
                (
                    self.sema
                        .enums_by_file_name
                        .get(&(root_file, symbol))
                        .copied()
                        .or_else(|| self.sema.resolve_builtin_enum_name(symbol)),
                    false,
                )
            }
            SemaTypeRootAuthority::GlobalSpeculative => (
                self.sema
                    .enum_id_for_name(symbol)
                    .or_else(|| self.sema.resolve_builtin_enum_name(symbol)),
                true,
            ),
        };
        let Some(id) = id else {
            return Ok(None);
        };
        let metadata = self
            .sema
            .type_pool
            .enum_metadata(id)
            .expect("type lookup names a registered enum identity");
        Ok(Some(self.type_fact(
            Type::new_enum(id),
            metadata.file_id,
            metadata.is_pub || bypass_visibility,
        )))
    }

    fn root_type_alias(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let SemaTypeRootAuthority::KnownFile(root_file) = self.root_authority else {
            return Ok(None);
        };
        let symbol = self.sema.interner.get_or_intern(name);
        self.sema.record_body_module_item_lookup(root_file, symbol);
        let mut value = self
            .sema
            .resolve_const_info_in_file(symbol, root_file)
            .map(|info| info.value);
        if value.is_none() && self.sema.declaration_binding_active {
            value = provider_failure(
                self.sema
                    .try_resolve_indexed_const_during_binding(symbol, root_file),
            )?;
        }
        let Some(ConstValue::Type(value)) = value else {
            return Ok(None);
        };
        let Some(info) = self.sema.resolve_const_info_in_file(symbol, root_file) else {
            return Ok(Some(self.type_fact(value, root_file, true)));
        };
        let (defining_file, is_public) = (info.span.file_id, info.is_pub);
        Ok(Some(self.type_fact(value, defining_file, is_public)))
    }

    fn module_struct_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let file = self.sema.module_registry.get_def(*module).file_id;
        let symbol = self.sema.interner.get_or_intern(name);
        self.sema.record_body_module_item_lookup(file, symbol);
        let Some(id) = self.sema.structs_by_file_name.get(&(file, symbol)).copied() else {
            return Ok(None);
        };
        let metadata = self
            .sema
            .type_pool
            .struct_metadata(id)
            .expect("qualified type lookup names a registered struct identity");
        Ok(Some(self.type_fact(
            Type::new_struct(id),
            metadata.file_id,
            metadata.is_pub,
        )))
    }

    fn module_enum_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let file = self.sema.module_registry.get_def(*module).file_id;
        let symbol = self.sema.interner.get_or_intern(name);
        self.sema.record_body_module_item_lookup(file, symbol);
        let Some(id) = self.sema.enums_by_file_name.get(&(file, symbol)).copied() else {
            return Ok(None);
        };
        let metadata = self
            .sema
            .type_pool
            .enum_metadata(id)
            .expect("qualified type lookup names a registered enum identity");
        Ok(Some(self.type_fact(
            Type::new_enum(id),
            metadata.file_id,
            metadata.is_pub,
        )))
    }

    fn module_type_alias(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<Type, FileId>>> {
        let file = self.sema.module_registry.get_def(*module).file_id;
        let symbol = self.sema.interner.get_or_intern(name);
        self.sema.record_body_module_item_lookup(file, symbol);
        if self.sema.declaration_binding_active && self.sema.value_const(&(file, symbol)).is_none()
        {
            provider_failure(
                self.sema
                    .try_resolve_indexed_const_during_binding(symbol, file),
            )?;
        }
        let Some(info) = self.sema.value_const(&(file, symbol)) else {
            return Ok(None);
        };
        let ConstValue::Type(value) = info.value else {
            return Ok(None);
        };
        Ok(Some(self.type_fact(value, info.span.file_id, info.is_pub)))
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
        self.root_authority != SemaTypeRootAuthority::GlobalSpeculative
    }

    fn allows_qualified_comptime_call_head(
        &self,
        _scope: &FileId,
        expectation: crate::SemanticComptimeCallExpectation,
    ) -> bool {
        self.root_authority != SemaTypeRootAuthority::GlobalSpeculative
            && !(self.resolution_context == SemaTypeResolutionContext::ArrayLength
                && expectation == crate::SemanticComptimeCallExpectation::Value)
    }

    fn resolve_array_length(
        &mut self,
        scope: &FileId,
        length: &ArrayLen,
    ) -> SemaProviderResult<Option<u64>> {
        provider_failure(self.resolve_array_length_fact(*scope, length)).map(Some)
    }

    fn array_type(&mut self, element: Type, length: Option<u64>) -> SemaProviderResult<Type> {
        Ok(Type::new_array(self.sema.get_or_create_array_type(
            element,
            length.expect("concrete type resolution always resolves array lengths"),
        )))
    }

    fn ptr_const_type(&mut self, pointee: Type) -> SemaProviderResult<Type> {
        Ok(Type::new_ptr_const(
            self.sema.type_pool.intern_ptr_const_from_type(pointee),
        ))
    }

    fn ptr_mut_type(&mut self, pointee: Type) -> SemaProviderResult<Type> {
        Ok(Type::new_ptr_mut(
            self.sema.type_pool.intern_ptr_mut_from_type(pointee),
        ))
    }

    fn preflight_slice(&mut self, _scope: &FileId, _syntax: &str) -> SemaProviderResult<()> {
        provider_failure(self.sema.require_preview(
            PreviewFeature::Slices,
            "the slice type `[T]`",
            self.span,
        ))
    }

    fn slice_type(
        &mut self,
        _scope: &FileId,
        syntax: &str,
        element: Type,
    ) -> SemaProviderResult<Type> {
        provider_failure(
            self.sema
                .get_or_create_slice_struct_from_element(syntax, element, self.span),
        )
    }

    fn builtin_type_call(
        &mut self,
        scope: &FileId,
        name: &str,
        arguments: &[String],
    ) -> SemaProviderResult<Option<Type>> {
        if name == "Str" {
            let capacity = match arguments {
                [argument] => provider_failure(
                    self.resolve_array_length_fact(*scope, &ArrayLen::Named(argument.to_string())),
                )?,
                _ => {
                    return provider_failure(Err(CompileError::new(
                        ErrorKind::UnknownType(format!("{}({})", name, arguments.join(", "))),
                        self.span,
                    )));
                }
            };
            self.sema.record_declaration_builtin_type_call_head(
                super::BuiltinTypeCallHead::FixedCapacityString,
            );
            provider_failure(
                self.sema
                    .get_or_create_str_fixed_struct(capacity, self.span)
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
        let symbol = self.sema.interner.get_or_intern(name);
        let mut key = match self.root_authority {
            SemaTypeRootAuthority::KnownFile(root_file) => {
                self.sema.resolve_function_name_local(symbol, root_file)
            }
            SemaTypeRootAuthority::GlobalSpeculative => Some(symbol),
        };
        if key.is_none()
            && self.sema.declaration_binding_active
            && let SemaTypeRootAuthority::KnownFile(root_file) = self.root_authority
        {
            let observer = self.sema.declaration_type_observer.clone();
            provider_failure(
                self.sema
                    .collect_free_function_signature_during_binding(symbol, Some(root_file)),
            )?;
            self.sema.declaration_type_observer = observer;
            key = self.sema.resolve_function_name_local(symbol, root_file);
        }
        let Some(key) = key else {
            return Ok(None);
        };
        let Some(info) = self.sema.functions.get(&key).copied() else {
            return Ok(None);
        };
        self.sema.record_declaration_type_call_head(key, info);
        let mut fact = self.constructor_fact(key, info);
        if self.root_authority == SemaTypeRootAuthority::GlobalSpeculative {
            fact.is_public = true;
        }
        Ok(Some(fact))
    }

    fn module_constructor(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        let file = self.sema.module_registry.get_def(*module).file_id;
        let symbol = self.sema.interner.get_or_intern(name);
        if self.sema.declaration_binding_active {
            let observer = self.sema.declaration_type_observer.clone();
            provider_failure(
                self.sema
                    .collect_free_function_signature_during_binding(symbol, Some(file)),
            )?;
            self.sema.declaration_type_observer = observer;
        }
        let Some(key) = self.sema.resolve_function_name_local(symbol, file) else {
            return Ok(None);
        };
        let Some(info) = self
            .sema
            .functions
            .get(&key)
            .copied()
            .filter(|info| info.file_id == file)
        else {
            return Ok(None);
        };
        self.sema.record_declaration_type_call_head(key, info);
        Ok(Some(self.constructor_fact(key, info)))
    }

    fn resolve_value_argument(
        &mut self,
        scope: &FileId,
        constructor: &str,
        _head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        _parameter_index: usize,
        _type_arguments: &[(Spur, Type)],
        _value_arguments: &[(Spur, ConstValue)],
        syntax: &str,
    ) -> SemaProviderResult<ConstValue> {
        match self.resolve_value_argument_fact(*scope, constructor, syntax) {
            Ok(value) => Ok(value),
            Err(error) if self.resolution_context == SemaTypeResolutionContext::ArrayLength => {
                let length = syntax
                    .parse::<u64>()
                    .map(ArrayLen::Literal)
                    .unwrap_or_else(|_| ArrayLen::Named(syntax.to_string()));
                provider_failure(
                    self.resolve_array_length_fact(*scope, &length)
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
        let type_arguments = type_arguments.iter().copied().collect::<HashMap<_, _>>();
        let value_arguments = value_arguments.iter().copied().collect::<HashMap<_, _>>();
        provider_failure(
            self.sema
                .reduce_type_ctor_body(head.key, &type_arguments, &value_arguments)
                .map_err(|error| Sema::<D>::label_ctor_instantiation_site(error, self.span))
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

impl<D: DeclarationPhase> SemaTypeSyntaxProvider<'_, '_, '_, D> {
    fn resolve_array_length_fact(
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
        scope: FileId,
        length: &ArrayLen,
    ) -> CompileResult<u64> {
        let ArrayLen::Named(name) = length else {
            let ArrayLen::Literal(value) = length else {
                unreachable!()
            };
            return Ok(*value);
        };
        if let Ok(value) = name.parse::<u64>() {
            return Ok(value);
        }
        if let Some((callee, arguments)) = parse_type_call_syntax(name) {
            return self.resolve_array_length_call_fact(scope, &callee, &arguments);
        }

        let symbol = self.sema.interner.get_or_intern(name);
        let value = if let Some(value) = self
            .value_substitutions
            .and_then(|substitutions| substitutions.get(&symbol))
        {
            *value
        } else if let SemaTypeRootAuthority::KnownFile(root_file) = self.root_authority {
            if let Some(info) = self
                .sema
                .resolve_const_info_in_file(symbol, root_file)
                .cloned()
            {
                self.sema.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::ValueConst {
                        file: info.span.file_id.index(),
                        name: name.to_owned(),
                    },
                );
                info.value
            } else if self.sema.declaration_binding_active
                && let Some(value) = self
                    .sema
                    .try_resolve_indexed_const_during_binding(symbol, root_file)?
            {
                self.sema.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::ValueConst {
                        file: root_file.index(),
                        name: name.to_owned(),
                    },
                );
                value
            } else {
                let hint = self.out_of_scope_const_hint(symbol, root_file);
                return Err(self.invalid_array_length(format!(
                    "'{name}' is not a compile-time constant; array lengths must be an integer literal, a `const`, or a `comptime` value parameter{hint}"
                )));
            }
        } else {
            return Err(self.invalid_array_length(format!(
                "'{name}' is not a compile-time constant; array lengths must be an integer literal, a `const`, or a `comptime` value parameter"
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

    fn resolve_array_length_call_fact(
        &mut self,
        scope: FileId,
        callee: &str,
        arguments: &[String],
    ) -> CompileResult<u64> {
        let call = crate::resolve_semantic_comptime_call(
            self,
            &scope,
            callee,
            arguments,
            crate::SemanticComptimeCallExpectation::Value,
        )
        .map_err(|failure| self.array_length_call_failure(callee, failure))?;
        match call.result {
            crate::SemanticComptimeCallResult::Value(ConstValue::Integer(value)) if value >= 0 => {
                u64::try_from(value).map_err(|_| {
                    self.invalid_array_length(format!(
                        "array length '{callee}(...)' ({value}) is too large"
                    ))
                })
            }
            crate::SemanticComptimeCallResult::Value(ConstValue::Integer(value)) => Err(self
                .invalid_array_length(format!(
                    "array length '{callee}(...)' is negative ({value})"
                ))),
            crate::SemanticComptimeCallResult::Value(_) => Err(self.invalid_array_length(format!(
                "array length call '{callee}(...)' did not evaluate to a compile-time integer"
            ))),
            crate::SemanticComptimeCallResult::Type(_) => {
                unreachable!("value call expectation rejects type-returning callees")
            }
        }
    }

    fn array_length_call_failure(
        &self,
        callee: &str,
        failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
    ) -> CompileError {
        use crate::SemanticResolutionError as E;
        use crate::SemanticTypeSyntaxFailure as F;

        match failure {
            E::ProviderAbort(error) => match error {},
            E::ProviderFailure(error) => error,
            E::Semantic(F::TypeWhereValueExpected { .. }) => self.invalid_array_length(format!(
                "array length call '{callee}(...)' must return a value, not a type"
            )),
            E::Semantic(F::InvalidConstructorArity { .. })
            | E::Semantic(F::RuntimeConstructorParameter { .. }) => self.invalid_array_length(
                format!(
                    "array length call '{callee}(...)' is not a compile-time constant; its callee must be a value-returning function whose parameters are all `comptime`"
                ),
            ),
            E::Semantic(F::ConstructorDidNotReduce { .. }) => self.invalid_array_length(format!(
                "array length call '{callee}(...)' did not evaluate to a compile-time integer"
            )),
            E::ComptimeCallTypeArgument { error, .. } => {
                self.sema.type_syntax_compile_error(*error, self.span)
            }
            E::Semantic(F::Path(_))
            | E::Semantic(F::UnknownType { .. })
            | E::Semantic(F::UnknownModuleMember { .. })
            | E::Semantic(F::PrivateItem { .. })
            | E::Semantic(F::AmbiguousItem { .. })
            | E::Semantic(F::NotTypeConstructor { .. }) => self.invalid_array_length(format!(
                "'{callee}' is not a function; array lengths must be an integer literal, a `const`, a `comptime` value parameter, or a call to a comptime function"
            )),
            E::Semantic(F::ValueWhereTypeExpected { argument, .. }) => self
                .invalid_array_length(format!(
                    "array length call '{callee}(...)' has non-type argument '{argument}' for a `type` parameter"
                )),
        }
    }

    fn invalid_array_length(&self, reason: String) -> CompileError {
        CompileError::new(ErrorKind::InvalidArrayLength { reason }, self.span)
    }

    /// Error-path-only candidate hint for a bare array-length name that is not
    /// in the referencing file's scope. Bare names resolve by ordinary scoped
    /// resolution (same module + visibility); a same-named constant in another
    /// module does not resolve merely because it is globally unique. When such
    /// constants exist elsewhere, name the modules that declare a visible
    /// (`pub`) integer constant of that name so the diagnostic can steer the
    /// author to an import + file-level `const` alias. The bounded scan runs
    /// only after the keyed lookup has already missed, so the happy path stays
    /// a single by-file lookup.
    fn out_of_scope_const_hint(&self, symbol: Spur, exclude: FileId) -> String {
        let mut modules: Vec<&str> = self
            .sema
            .value_consts()
            .filter(|((file, name), info)| {
                *file != exclude
                    && *name == symbol
                    && info.is_pub
                    && info.value.as_int_value().is_some()
            })
            .filter_map(|((file, _), _)| self.sema.file_paths.get(file).map(String::as_str))
            .collect();
        modules.sort_unstable();
        modules.dedup();
        match modules.as_slice() {
            [] => String::new(),
            _ => format!(
                "; an integer constant of that name is declared in {} — import that module and bind a file-level `const` (for example `const {}: i32 = <module>.{};`) to use it as an array length here",
                modules.join(", "),
                self.sema.interner.resolve(&symbol),
                self.sema.interner.resolve(&symbol),
            ),
        }
    }

    fn observe_materialized_type_fact(&mut self, ty: Type) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self
                    .sema
                    .type_pool
                    .struct_metadata(id)
                    .expect("resolved declaration type names a registered struct identity");
                if !def.is_builtin && !def.name.starts_with("__anon_struct_") {
                    self.observe_type_dependency(
                        def.file_id,
                        def.name,
                        super::DeclarationTypeDependencyTargetKind::Struct,
                    );
                }
            }
            TypeKind::Enum(id) => {
                let def = self
                    .sema
                    .type_pool
                    .enum_metadata(id)
                    .expect("resolved declaration type names a registered enum identity");
                self.observe_type_dependency(
                    def.file_id,
                    def.name,
                    super::DeclarationTypeDependencyTargetKind::Enum,
                );
            }
            TypeKind::Array(id) => {
                self.observe_materialized_type_fact(self.sema.type_pool.array_def(id).0)
            }
            TypeKind::PtrConst(id) => {
                self.observe_materialized_type_fact(self.sema.type_pool.ptr_const_def(id));
            }
            TypeKind::PtrMut(id) => {
                self.observe_materialized_type_fact(self.sema.type_pool.ptr_mut_def(id));
            }
            _ => {}
        }
    }

    fn flush_observed_type_dependencies(&mut self) {
        for (file, name, kind) in std::mem::take(&mut self.observed_type_dependencies) {
            self.sema
                .record_resolved_declaration_type_target(file, name, kind);
        }
    }

    fn observe_type_dependency(
        &mut self,
        file: FileId,
        name: String,
        kind: super::DeclarationTypeDependencyTargetKind,
    ) {
        let dependency = (file, name, kind);
        if !self.observed_type_dependencies.contains(&dependency) {
            self.observed_type_dependencies.push(dependency);
        }
    }

    fn resolve_value_argument_fact(
        &mut self,
        _scope: FileId,
        constructor: &str,
        syntax: &str,
    ) -> CompileResult<ConstValue> {
        let text = syntax.trim();
        if let Ok(value) = text.parse::<i128>() {
            return Ok(ConstValue::Integer(value));
        }
        if text == "true" {
            return Ok(ConstValue::Bool(true));
        }
        if text == "false" {
            return Ok(ConstValue::Bool(false));
        }
        let symbol = self.sema.interner.get_or_intern(text);
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
        if let SemaTypeRootAuthority::KnownFile(root_file) = self.root_authority
            && let Some(info) = self.sema.resolve_const_info_in_file(symbol, root_file)
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
    fn module_binding_fact(
        &self,
        binding: super::info::ConstInfo,
    ) -> Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>> {
        let target = binding.ty.as_module()?;
        let defining_file = self
            .sema
            .get_file_path(binding.span.file_id)
            .unwrap_or("<unknown>");
        Some(crate::SemanticModuleBinding {
            target,
            site: binding.span.file_id,
            is_public: binding.is_pub,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(
                self.sema.get_file_path(binding.span.file_id),
            ),
            defining_file: std::sync::Arc::from(defining_file),
        })
    }

    fn type_fact(
        &self,
        value: Type,
        defining_file_id: FileId,
        is_public: bool,
    ) -> crate::SemanticTypeFact<Type, FileId> {
        let defining_file = self
            .sema
            .get_file_path(defining_file_id)
            .unwrap_or("<unknown>");
        crate::SemanticTypeFact {
            value,
            site: defining_file_id,
            is_public,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(
                self.sema.get_file_path(defining_file_id),
            ),
            defining_file: std::sync::Arc::from(defining_file),
        }
    }

    fn constructor_fact(
        &self,
        key: Spur,
        info: FunctionInfo,
    ) -> crate::SemanticTypeConstructorHead<Spur, Spur, FileId> {
        let names = self.sema.param_arena.names(info.params);
        let comptime = self.sema.param_arena.comptime(info.params);
        let type_parameters = self.sema.comptime_type_param_flags(&info);
        let parameters = names
            .iter()
            .zip(comptime)
            .zip(type_parameters)
            .map(
                |((&name, &is_comptime), is_type)| crate::SemanticTypeConstructorParameter {
                    name,
                    is_comptime,
                    is_type,
                },
            )
            .collect::<Vec<_>>();
        let defining_file = self.sema.get_file_path(info.file_id).unwrap_or("<unknown>");
        crate::SemanticTypeConstructorHead {
            key,
            site: info.file_id,
            parameters: parameters.into(),
            returns_type: self.sema.function_returns_type(&info),
            is_public: info.is_pub,
            defining_domain: crate::SemanticVisibilityDomain::from_file_path(
                self.sema.get_file_path(info.file_id),
            ),
            defining_file: std::sync::Arc::from(defining_file),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeferredTypeResolution {
    Resolved(Type),
    Pending,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeferredValueResolution {
    Resolved(ConstValue),
    Pending,
}

struct DeferredSemaTypeSyntaxProvider<'s, 'a, 'c, D: DeclarationPhase> {
    inner: SemaTypeSyntaxProvider<'s, 'a, 'c, D>,
    type_params: &'c [Spur],
    value_params: &'c [Spur],
    value_param_type_syms: &'c [(Spur, Spur)],
}

impl<D: DeclarationPhase> crate::SemanticModulePathProvider<FileId, crate::types::ModuleId, FileId>
    for DeferredSemaTypeSyntaxProvider<'_, '_, '_, D>
{
    type Abort = Infallible;
    type Failure = CompileError;

    fn root_module_binding(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        self.inner.root_module_binding(scope, name)
    }

    fn module_binding(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticModuleBinding<crate::types::ModuleId, FileId>>>
    {
        self.inner.module_binding(module, name)
    }

    fn module_display_name(&self, module: &crate::types::ModuleId) -> std::sync::Arc<str> {
        self.inner.module_display_name(module)
    }

    fn accessing_domain(&self, scope: &FileId) -> crate::SemanticVisibilityDomain {
        self.inner.accessing_domain(scope)
    }
}

impl<D: DeclarationPhase>
    crate::SemanticTypeSyntaxProvider<
        FileId,
        crate::types::ModuleId,
        FileId,
        Spur,
        Spur,
        DeferredTypeResolution,
        DeferredValueResolution,
    > for DeferredSemaTypeSyntaxProvider<'_, '_, '_, D>
{
    fn substituted_type(
        &mut self,
        _scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        let Some(symbol) = self.inner.sema.interner.get(name) else {
            return Ok(None);
        };
        if self.type_params.contains(&symbol) {
            return Ok(Some(DeferredTypeResolution::Pending));
        }
        if self.value_params.contains(&symbol) {
            return provider_failure(Err(CompileError::new(
                ErrorKind::UnknownType(name.to_string()),
                self.inner.span,
            )));
        }
        Ok(None)
    }

    fn primitive_type(&mut self, name: &str) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        self.inner
            .primitive_type(name)
            .map(|value| value.map(DeferredTypeResolution::Resolved))
    }

    fn builtin_type(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        self.inner
            .builtin_type(scope, name)
            .map(|value| value.map(DeferredTypeResolution::Resolved))
    }

    fn root_struct_type(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.root_struct_type(scope, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn root_enum_type(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.root_enum_type(scope, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn root_type_alias(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.root_type_alias(scope, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn module_struct_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.module_struct_type(module, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn module_enum_type(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.module_enum_type(module, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn module_type_alias(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeFact<DeferredTypeResolution, FileId>>> {
        self.inner.module_type_alias(module, name).map(|fact| {
            fact.map(|fact| crate::SemanticTypeFact {
                value: DeferredTypeResolution::Resolved(fact.value),
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain,
                defining_file: fact.defining_file,
            })
        })
    }

    fn observe_selected_named_type(
        &mut self,
        name: &str,
        kind: crate::SemanticTypeFactKind,
        fact: &crate::SemanticTypeFact<DeferredTypeResolution, FileId>,
    ) -> SemaProviderResult<()> {
        if let DeferredTypeResolution::Resolved(value) = fact.value {
            let resolved = crate::SemanticTypeFact {
                value,
                site: fact.site,
                is_public: fact.is_public,
                defining_domain: fact.defining_domain.clone(),
                defining_file: fact.defining_file.clone(),
            };
            self.inner
                .observe_selected_named_type(name, kind, &resolved)?;
        }
        Ok(())
    }

    fn observe_materialized_type(&mut self, ty: &DeferredTypeResolution) -> SemaProviderResult<()> {
        if let DeferredTypeResolution::Resolved(ty) = ty {
            self.inner.observe_materialized_type(ty)?;
        }
        Ok(())
    }

    fn allows_qualified_paths(&self, scope: &FileId) -> bool {
        self.inner.allows_qualified_paths(scope)
    }

    fn allows_qualified_comptime_call_head(
        &self,
        scope: &FileId,
        expectation: crate::SemanticComptimeCallExpectation,
    ) -> bool {
        expectation != crate::SemanticComptimeCallExpectation::Value
            && self
                .inner
                .allows_qualified_comptime_call_head(scope, expectation)
    }

    fn resolve_array_length(
        &mut self,
        _scope: &FileId,
        length: &ArrayLen,
    ) -> SemaProviderResult<Option<u64>> {
        if let ArrayLen::Literal(length) = length {
            return Ok(Some(*length));
        }
        let ArrayLen::Named(name) = length else {
            unreachable!()
        };
        let value = provider_failure(self.inner.sema.validate_deferred_value_position(
            name,
            self.type_params,
            self.value_params,
            self.value_param_type_syms,
            None,
            None,
            true,
            self.inner.span,
        ))?;
        match value {
            None => Ok(None),
            Some(ConstValue::Integer(value)) => u64::try_from(value).map(Some).map_err(|_| {
                crate::SemanticProviderError::Failure(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!("array length must be non-negative, got {value}"),
                    },
                    self.inner.span,
                ))
            }),
            Some(_) => provider_failure(Err(CompileError::new(
                ErrorKind::InvalidArrayLength {
                    reason: "array length must be an integer".to_string(),
                },
                self.inner.span,
            ))),
        }
    }

    fn array_type(
        &mut self,
        element: DeferredTypeResolution,
        length: Option<u64>,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match (element, length) {
            (DeferredTypeResolution::Resolved(element), Some(length)) => self
                .inner
                .array_type(element, Some(length))
                .map(DeferredTypeResolution::Resolved),
            _ => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn ptr_const_type(
        &mut self,
        pointee: DeferredTypeResolution,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match pointee {
            DeferredTypeResolution::Resolved(pointee) => self
                .inner
                .ptr_const_type(pointee)
                .map(DeferredTypeResolution::Resolved),
            DeferredTypeResolution::Pending => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn ptr_mut_type(
        &mut self,
        pointee: DeferredTypeResolution,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match pointee {
            DeferredTypeResolution::Resolved(pointee) => self
                .inner
                .ptr_mut_type(pointee)
                .map(DeferredTypeResolution::Resolved),
            DeferredTypeResolution::Pending => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn preflight_slice(&mut self, scope: &FileId, syntax: &str) -> SemaProviderResult<()> {
        self.inner.preflight_slice(scope, syntax)
    }

    fn slice_type(
        &mut self,
        scope: &FileId,
        syntax: &str,
        element: DeferredTypeResolution,
    ) -> SemaProviderResult<DeferredTypeResolution> {
        match element {
            DeferredTypeResolution::Resolved(element) => self
                .inner
                .slice_type(scope, syntax, element)
                .map(DeferredTypeResolution::Resolved),
            DeferredTypeResolution::Pending => Ok(DeferredTypeResolution::Pending),
        }
    }

    fn builtin_type_call(
        &mut self,
        scope: &FileId,
        name: &str,
        arguments: &[String],
    ) -> SemaProviderResult<Option<DeferredTypeResolution>> {
        self.inner
            .builtin_type_call(scope, name, arguments)
            .map(|value| value.map(DeferredTypeResolution::Resolved))
    }

    fn root_constructor(
        &mut self,
        scope: &FileId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        self.inner.root_constructor(scope, name)
    }

    fn module_constructor(
        &mut self,
        module: &crate::types::ModuleId,
        name: &str,
    ) -> SemaProviderResult<Option<crate::SemanticTypeConstructorHead<Spur, Spur, FileId>>> {
        self.inner.module_constructor(module, name)
    }

    fn resolve_value_argument(
        &mut self,
        _scope: &FileId,
        constructor: &str,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        parameter_index: usize,
        type_arguments: &[(Spur, DeferredTypeResolution)],
        value_arguments: &[(Spur, DeferredValueResolution)],
        syntax: &str,
    ) -> SemaProviderResult<DeferredValueResolution> {
        let function = *self
            .inner
            .sema
            .functions
            .get(&head.key)
            .expect("selected constructor has function metadata");
        let param_types = self.inner.sema.param_arena.types(function.params);
        let rir_param_types = self
            .inner
            .sema
            .rir
            .params(function.rir_params(self.inner.sema.rir))
            .iter()
            .map(|parameter| parameter.ty)
            .collect::<Vec<_>>();
        let callee_types = type_arguments
            .iter()
            .filter_map(|(name, value)| match value {
                DeferredTypeResolution::Resolved(value) => Some((*name, *value)),
                DeferredTypeResolution::Pending => None,
            })
            .collect::<HashMap<_, _>>();
        let callee_values = value_arguments
            .iter()
            .filter_map(|(name, value)| match value {
                DeferredValueResolution::Resolved(value) => Some((*name, *value)),
                DeferredValueResolution::Pending => None,
            })
            .collect::<HashMap<_, _>>();
        let expected = if param_types[parameter_index] != Type::COMPTIME_TYPE {
            Some(param_types[parameter_index])
        } else if let Some(bound) = callee_types.get(&rir_param_types[parameter_index]) {
            Some(*bound)
        } else {
            let callee_type_params = head
                .parameters
                .iter()
                .filter_map(|parameter| parameter.is_type.then_some(parameter.name))
                .collect::<Vec<_>>();
            let callee_value_params = head
                .parameters
                .iter()
                .filter_map(|parameter| (!parameter.is_type).then_some(parameter.name))
                .collect::<Vec<_>>();
            let parameter_type_name = self
                .inner
                .sema
                .interner
                .resolve(&rir_param_types[parameter_index]);
            if self.inner.sema.deferred_signature_substitutions_are_ready(
                parameter_type_name,
                &callee_type_params,
                &callee_value_params,
                &callee_types,
                &callee_values,
            ) {
                Some(provider_failure(
                    self.inner.sema.resolve_substituted_param_type(
                        &function,
                        parameter_index,
                        param_types[parameter_index],
                        &callee_types,
                        &callee_values,
                    ),
                )?)
            } else {
                None
            }
        };
        let contract = Some((
            self.inner.sema.interner.get_or_intern(constructor),
            head.parameters[parameter_index].name,
        ));
        provider_failure(self.inner.sema.validate_deferred_value_position(
            syntax,
            self.type_params,
            self.value_params,
            self.value_param_type_syms,
            expected,
            contract,
            false,
            self.inner.span,
        ))
        .map(|value| match value {
            Some(value) => DeferredValueResolution::Resolved(value),
            None => DeferredValueResolution::Pending,
        })
    }

    fn reduce_comptime_call(
        &mut self,
        head: &crate::SemanticTypeConstructorHead<Spur, Spur, FileId>,
        type_arguments: &[(Spur, DeferredTypeResolution)],
        value_arguments: &[(Spur, DeferredValueResolution)],
    ) -> SemaProviderResult<
        Option<crate::SemanticComptimeCallResult<DeferredTypeResolution, DeferredValueResolution>>,
    > {
        if type_arguments
            .iter()
            .any(|(_, value)| *value == DeferredTypeResolution::Pending)
            || value_arguments
                .iter()
                .any(|(_, value)| *value == DeferredValueResolution::Pending)
        {
            return Ok(Some(if head.returns_type {
                crate::SemanticComptimeCallResult::Type(DeferredTypeResolution::Pending)
            } else {
                crate::SemanticComptimeCallResult::Value(DeferredValueResolution::Pending)
            }));
        }
        let resolved_types = type_arguments
            .iter()
            .map(|(name, value)| match value {
                DeferredTypeResolution::Resolved(value) => (*name, *value),
                DeferredTypeResolution::Pending => unreachable!(),
            })
            .collect::<Vec<_>>();
        let resolved_values = value_arguments
            .iter()
            .map(|(name, value)| match value {
                DeferredValueResolution::Resolved(value) => (*name, *value),
                DeferredValueResolution::Pending => unreachable!(),
            })
            .collect::<Vec<_>>();
        self.inner
            .reduce_comptime_call(head, &resolved_types, &resolved_values)
            .map(|result| {
                result.map(|result| match result {
                    crate::SemanticComptimeCallResult::Type(value) => {
                        crate::SemanticComptimeCallResult::Type(DeferredTypeResolution::Resolved(
                            value,
                        ))
                    }
                    crate::SemanticComptimeCallResult::Value(value) => {
                        crate::SemanticComptimeCallResult::Value(DeferredValueResolution::Resolved(
                            value,
                        ))
                    }
                })
            })
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

fn module_path_resolution_compile_error(
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

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    /// Get a human-readable name for a type.
    pub(crate) fn format_type_name(&self, ty: Type) -> String {
        // A constructor-produced anonymous type prints its instantiation
        // spelling (`ArrayBuf(i64)`), not its internal structural name
        // (RUE-610; recorded by `reduce_type_ctor_body`).
        if let Some(display) = self.ctor_type_displays.get(&ty) {
            return display.clone();
        }
        match ty.kind() {
            TypeKind::I8 => "i8".to_string(),
            TypeKind::I16 => "i16".to_string(),
            TypeKind::I32 => "i32".to_string(),
            TypeKind::I64 => "i64".to_string(),
            TypeKind::U8 => "u8".to_string(),
            TypeKind::U16 => "u16".to_string(),
            TypeKind::U32 => "u32".to_string(),
            TypeKind::U64 => "u64".to_string(),
            TypeKind::Bool => "bool".to_string(),
            TypeKind::Unit => "()".to_string(),
            TypeKind::Never => "!".to_string(),
            TypeKind::Error => "<error>".to_string(),
            // Nominal strings and generated string views are ordinary structs.
            TypeKind::Struct(struct_id) => {
                self.type_pool
                    .struct_metadata(struct_id)
                    .expect("struct type must have declaration metadata")
                    .name
            }
            TypeKind::Enum(enum_id) => {
                self.type_pool
                    .enum_metadata(enum_id)
                    .expect("enum type must have declaration metadata")
                    .name
            }
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                format!("[{}; {}]", self.format_type_name(element_type), length)
            }
            TypeKind::PtrConst(ptr_id) => {
                let pointee = self.type_pool.ptr_const_def(ptr_id);
                format!("ptr const {}", self.format_type_name(pointee))
            }
            TypeKind::PtrMut(ptr_id) => {
                let pointee = self.type_pool.ptr_mut_def(ptr_id);
                format!("ptr mut {}", self.format_type_name(pointee))
            }
            TypeKind::Module(_) => "<module>".to_string(),
            TypeKind::ComptimeType => "type".to_string(),
        }
    }

    /// Point a container well-formedness rejection (E0499, RUE-388/RUE-646) at
    /// the user's type-constructor application. The gate raises inside the
    /// constructor's own body (`@require_droppable(T)` in std source), so
    /// without this label the user's file never appears in the diagnostic
    /// (RUE-610). Other reduction errors already carry their own spans.
    pub(crate) fn label_ctor_instantiation_site(err: CompileError, span: Span) -> CompileError {
        if matches!(err.kind, ErrorKind::ContainerElementIsLinear { .. }) {
            err.with_label("required by the type-constructor application here", span)
        } else {
            err
        }
    }

    /// Check if a type is a Copy type.
    /// This differs from Type::is_copy() because it can look up struct definitions
    /// to check if a struct is marked with @copy.
    pub(crate) fn is_type_copy(&self, ty: Type) -> bool {
        match ty.kind() {
            // Primitive Copy types
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Unit => true,
            // An enum is Copy iff every variant payload is Copy (RUE-221:
            // enum multiplicity is the join of its variants' payload
            // multiplicities, lattice Copy ⊑ Affine ⊑ Linear). A
            // discriminant-only (C-like) enum has no payloads and is Copy.
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
                enum_def
                    .variant_payloads
                    .iter()
                    .flatten()
                    .all(|&ty| self.is_type_copy(ty))
            }
            // Never and Error are Copy for convenience
            TypeKind::Never | TypeKind::Error => true,
            // Struct types: check if marked with @copy
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_copy
            }
            // Note: String is now handled via TypeKind::Struct with is_builtin
            // Arrays are Copy if their element type is Copy
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.is_type_copy(element_type)
            }
            // Module types are Copy (they're just compile-time namespace references)
            TypeKind::Module(_) => true,
            // ComptimeType is Copy (only exists at comptime anyway)
            TypeKind::ComptimeType => true,
            // Pointer types are Copy (they're just addresses)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => true,
        }
    }

    /// A `type` value or a module value has no runtime representation and so
    /// cannot be interned as an array element (`type` values: spec 4.14:6;
    /// modules: spec 10.4:145). An array with such an element decays to
    /// `<error>` during type resolution; sema then rejects the offending
    /// element with a clean diagnostic (E1200 for a `type` value, E0206 for a
    /// module) rather than reaching the intern pool, which panics on both kinds
    /// (RUE-253, RUE-265).
    pub(crate) fn is_non_internable_element(elem_ty: Type) -> bool {
        matches!(elem_ty.kind(), TypeKind::ComptimeType | TypeKind::Module(_))
    }

    /// Convert a fully-resolved InferType to a concrete Type.
    ///
    /// This handles the conversion of InferType::Array to Type::new_array(id)
    /// by using the array type registry.
    pub(crate) fn infer_type_to_type(&mut self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default (shouldn't happen after resolution)
            InferType::Array { element, length } => {
                // Recursively convert element type
                let elem_ty = self.infer_type_to_type(element);
                // A comptime-only (`type`) or module element cannot be interned;
                // leave the array as `<error>` so sema rejects it with a clean
                // diagnostic rather than panicking in the intern pool. `<error>`
                // elements decay the same way (RUE-253, RUE-265).
                if elem_ty == Type::ERROR || Self::is_non_internable_element(elem_ty) {
                    return Type::ERROR;
                }
                // Get or create the array type ID
                let array_type_id = self.get_or_create_array_type(elem_ty, *length);
                Type::new_array(array_type_id)
            }
        }
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    ///
    /// This handles the conversion of Type::new_array(id) to InferType::Array
    /// by looking up the array definition to get element type and length.
    pub(crate) fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty.kind() {
            TypeKind::Array(array_id) => {
                let (element_type, length) = self.type_pool.array_def(array_id);
                let element_infer = self.type_to_infer_type(element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length,
                }
            }
            // All other types wrap directly
            _ => InferType::Concrete(ty),
        }
    }
    /// Get (or lazily create) the synthetic 2-field struct that represents the
    /// second-class slice type `[T]` (ADR-0043, RUE-322).
    ///
    /// A slice is a fat pointer `{ ptr: ptr const T, len: u64 }`. Rather than
    /// invent a new `TypeKind`, the slice is modeled as a `@copy` synthetic
    /// struct so it flows through the existing multi-slot aggregate ABI, field
    /// reads (for `.len()`), and by-ref (`borrow`) parameter passing — exactly
    /// like the builtin `String` fat pointer. The struct is keyed by its slice
    /// syntax name (`[i32]`), so repeated references share one `StructId`.
    ///
    /// The shared syntax driver resolves the element before invoking this
    /// materializer. The pointer field is `ptr const T` so that `s[i]` can lower to
    /// `@ptr_read(@ptr_offset(ptr, i))` with the correct element stride.
    pub(crate) fn get_or_create_slice_struct_from_element(
        &mut self,
        type_name: &str,
        element_ty: Type,
        _span: Span,
    ) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};

        let type_sym = self.interner.get_or_intern(type_name);
        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }

        let ptr_type_id = self.type_pool.intern_ptr_const_from_type(element_ty);
        let ptr_ty = Type::new_ptr_const(ptr_type_id);

        let struct_def = StructDef {
            name: type_name.to_string(),
            fields: vec![
                StructField {
                    name: "ptr".to_string(),
                    ty: ptr_ty,
                },
                StructField {
                    name: "len".to_string(),
                    ty: Type::U64,
                },
            ],
            // A slice is a copyable view (no ownership of the backing store).
            is_copy: true,
            is_linear: false,
            destructor: None,
            // Marked builtin so it never participates in user drop-glue etc.
            is_builtin: true,
            is_pub: true,
            file_id: rue_span::FileId::new(0),
        };
        let (struct_id, _) = self.type_pool.register_struct(type_sym, struct_def);
        self.generated_structs.insert(type_sym, struct_id);
        Ok(Type::new_struct(struct_id))
    }

    /// Get (or lazily create) the synthetic 2-field struct that represents the
    /// `str` string type (ADR-0043 Phase 3, RUE-324).
    ///
    /// `str` is `[u8]` + the UTF-8 byte-string convention (ADR-0035): a
    /// read-only slice of bytes `{ ptr: ptr const u8, len: u64 }`, sharing the
    /// slice fat-pointer representation so `.len()` and byte-indexing `s[i]`
    /// reuse the slice machinery verbatim. Unlike a plain `[T]` slice, `str` is
    /// **first-class**: string literals are static-backed (their bytes live in
    /// `.rodata`, which cannot dangle), so a `str` value is storable, `Copy`,
    /// and reassignable — it is exempt from the second-class-escape rule. The
    /// struct is keyed by the name `str`, so every reference shares one
    /// `StructId`.
    pub(crate) fn get_or_create_str_struct(&mut self, span: Span) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};

        let type_sym = self.interner.get_or_intern("str");
        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }

        let ptr_type_id = self.type_pool.intern_ptr_const_from_type(Type::U8);
        let ptr_ty = Type::new_ptr_const(ptr_type_id);

        let struct_def = StructDef {
            name: "str".to_string(),
            fields: vec![
                StructField {
                    name: "ptr".to_string(),
                    ty: ptr_ty,
                },
                StructField {
                    name: "len".to_string(),
                    ty: Type::U64,
                },
            ],
            // `str` is a copyable, static-backed view (no ownership of bytes).
            is_copy: true,
            is_linear: false,
            destructor: None,
            // Builtin so it never participates in user drop-glue etc.
            is_builtin: true,
            is_pub: true,
            file_id: rue_span::FileId::new(0),
        };
        let (struct_id, _) = self.type_pool.register_struct(type_sym, struct_def);
        self.generated_structs.insert(type_sym, struct_id);
        let _ = span;
        Ok(Type::new_struct(struct_id))
    }

    /// Requalify destructor symbols for struct names that span files
    /// (RUE-571). Registration (`register_destructor`) runs per-file before
    /// ambiguity is knowable, so it records the bare `Type.__drop`; once
    /// declarations are complete this re-points colliding entries at their
    /// file-qualified form, so every consumer of `StructDef::destructor`
    /// (drop glue in CFG build and both codegen backends) agrees with the
    /// definition side, which builds its name via [`Self::destructor_symbol`].
    pub(crate) fn requalify_colliding_destructor_symbols(&mut self) {
        let ids: Vec<StructId> = self
            .structs_by_file_name
            .values()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        for struct_id in ids {
            if self.type_pool.struct_def(struct_id).destructor.is_none() {
                continue;
            }
            let qualified = self.destructor_symbol(struct_id);
            let def = self.type_pool.struct_def(struct_id);
            if def.destructor.as_deref() != Some(qualified.as_str()) {
                self.type_pool
                    .requalify_struct_destructor(struct_id, qualified);
            }
        }
    }

    /// The type-name component of function symbols for `struct_id` (RUE-571):
    /// delegates to [`TypeInternPool::struct_symbol_name`], the single
    /// definition shared with the drop-glue generator and both codegen
    /// backends.
    pub(crate) fn symbol_type_name(&self, struct_id: StructId) -> String {
        self.type_pool.struct_symbol_name(struct_id)
    }

    /// Symbol for a method (`Type.method`) or associated function
    /// (`Type::method`) of `struct_id`. Named user types are unconditionally
    /// file-qualified (ADR-0066, RUE-1089), while builtins remain bare.
    /// Definition sites (function-body analysis) and call sites (method /
    /// assoc-fn call analysis) must both build the name through this helper so
    /// they meet in AIR/codegen.
    pub(crate) fn method_symbol(
        &self,
        struct_id: StructId,
        method: &str,
        has_self: bool,
    ) -> String {
        let type_name = self.symbol_type_name(struct_id);
        if has_self {
            format!("{}.{}", type_name, method)
        } else {
            format!("{}::{}", type_name, method)
        }
    }

    /// Symbol for the destructor of `struct_id` (`Type.__drop`). Named user
    /// types are unconditionally file-qualified (ADR-0066, RUE-1089), while
    /// builtins remain bare.
    pub(crate) fn destructor_symbol(&self, struct_id: StructId) -> String {
        format!("{}.__drop", self.symbol_type_name(struct_id))
    }

    /// Is `ty` the synthetic `str` struct (ADR-0043 Phase 3, RUE-324)? Detected
    /// by the struct name being exactly `str`. Used to route string literals and
    /// slice-style `.len()`/index operations through the fat-pointer paths while
    /// keeping `str` first-class (exempt from the slice second-class rule).
    pub(crate) fn is_str_struct(&self, ty: Type) -> bool {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            self.type_pool.struct_def(struct_id).name == "str"
        } else {
            false
        }
    }

    /// Get (or lazily create) the synthetic 2-field struct representing a
    /// fixed-capacity string `Str(N)` (ADR-0043 Phase 5, RUE-326).
    ///
    /// `Str(N)` is the **fixed string rung** — `[u8; N]` + the UTF-8
    /// byte-string convention (ADR-0035) — the string analogue of the fixed
    /// array `[T; N]`: no heap, no allocator, a value type holding up to `N`
    /// bytes plus a current byte length. Each distinct capacity is its own
    /// named struct (`Str(8)`, keyed by that canonical name) so every reference
    /// to the same capacity shares one `StructId`.
    ///
    /// Representation note (this phase): `Str(N)` reuses the `str` fat-pointer
    /// shape `{ ptr: ptr const u8, len: u64 }`, and construction from a string
    /// literal is currently static-backed (bytes in `.rodata`, which cannot
    /// dangle). This makes `.len()`, byte-indexing, and coercion to `str` reuse
    /// the `str` machinery verbatim while the capacity `N` enforces the
    /// literal-fits legality rule. Genuine inline-stack storage and mutation
    /// (`push`/append) are deferred; for the literal-only, immutable surface of
    /// this phase the two are observationally identical.
    pub(crate) fn get_or_create_str_fixed_struct(
        &mut self,
        capacity: u64,
        span: Span,
    ) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};

        let name = format!("Str({})", capacity);
        let type_sym = self.interner.get_or_intern(&name);
        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }

        let ptr_type_id = self.type_pool.intern_ptr_const_from_type(Type::U8);
        let ptr_ty = Type::new_ptr_const(ptr_type_id);

        let struct_def = StructDef {
            name,
            fields: vec![
                StructField {
                    name: "ptr".to_string(),
                    ty: ptr_ty,
                },
                StructField {
                    name: "len".to_string(),
                    ty: Type::U64,
                },
            ],
            is_copy: true,
            is_linear: false,
            destructor: None,
            is_builtin: true,
            is_pub: true,
            file_id: rue_span::FileId::new(0),
        };
        let (struct_id, _) = self.type_pool.register_struct(type_sym, struct_def);
        self.generated_structs.insert(type_sym, struct_id);
        let _ = span;
        Ok(Type::new_struct(struct_id))
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
            let name = &self.type_pool.struct_def(struct_id).name;
            let digits = name.strip_prefix("Str(")?.strip_suffix(')')?;
            digits.parse::<u64>().ok()
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

    /// If `ty` is a synthetic slice struct `[T]` (ADR-0043, RUE-322), return its
    /// element type `T`; otherwise `None`. Detected by the struct name being
    /// slice syntax (`[..]` that is not a fixed-array `[T; N]`), the same naming
    /// trick used for anonymous structs.
    pub(crate) fn slice_element_type(&self, ty: Type) -> Option<Type> {
        if let TypeKind::Struct(struct_id) = ty.kind() {
            let def = self.type_pool.struct_def(struct_id);
            // A `[T]` slice, or the `str` string type which is `[u8]` + UTF-8
            // (ADR-0043 Phase 3, RUE-324): both are `{ptr: ptr const E, len}`
            // fat pointers, so `.len()` and `s[i]` reuse one path.
            let is_slice = def.name.starts_with('[')
                && def.name.ends_with(']')
                && parse_array_type_syntax(&def.name).is_none();
            // A fixed-capacity `Str(N)` (ADR-0043 Phase 5, RUE-326) is `[u8; N]`
            // + UTF-8 and shares the `{ptr, len}` shape, so `.len()` and byte
            // indexing route through the same slice/str path as `str`.
            let is_str_fixed = def.name.starts_with("Str(") && def.name.ends_with(')');
            if is_slice || def.name == "str" || is_str_fixed {
                // Field 0 is `ptr const T`; recover T from its pointee.
                if let TypeKind::PtrConst(ptr_id) = def.fields[0].ty.kind() {
                    return Some(self.type_pool.ptr_const_def(ptr_id));
                }
            }
        }
        None
    }

    /// Is `type_sym` the syntax of a slice type `[T]` (as opposed to a fixed
    /// array `[T; N]`)? Slices are second-class (ADR-0037, ADR-0043, RUE-322):
    /// callers use this to reject a slice type in a non-argument position
    /// (return / struct field / `let` / `const`) with a targeted diagnostic
    /// before the generic `resolve_type` path would report it.
    pub(crate) fn is_slice_type_syntax(&self, type_sym: Spur) -> bool {
        let name = self.interner.resolve(&type_sym);
        name.starts_with('[') && name.ends_with(']') && parse_array_type_syntax(name).is_none()
    }

    /// Reject a slice type appearing outside argument position. When `type_sym`
    /// is slice syntax and `--preview slices` is enabled, this returns the
    /// second-class-escape error `kind` (E0487/E0488/E0489); otherwise it
    /// returns `Ok(())` and the caller proceeds. The preview gate is checked
    /// first so that, without the flag, the user still sees the uniform
    /// "requires preview feature" message rather than a bespoke slice error.
    pub(crate) fn reject_slice_escape(
        &self,
        type_sym: Spur,
        span: Span,
        kind: ErrorKind,
    ) -> CompileResult<()> {
        if self.is_slice_type_syntax(type_sym) {
            self.require_preview(PreviewFeature::Slices, "the slice type `[T]`", span)?;
            return Err(CompileError::new(kind, span));
        }
        Ok(())
    }

    pub(crate) fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        self.resolve_type_inner(type_sym, span)
    }

    fn resolve_type_inner(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let syntax = self.interner.resolve(&type_sym).to_string();
        self.resolve_type_syntax_in_scope(&syntax, span.file_id, span, None)
    }

    fn resolve_type_syntax_in_scope(
        &mut self,
        syntax: &str,
        root_file: FileId,
        span: Span,
        context: Option<&AnalysisContext>,
    ) -> CompileResult<Type> {
        let (type_substitutions, value_substitutions) = context
            .map(|context| (&context.comptime_type_vars, &context.comptime_value_vars))
            .unzip();
        self.resolve_type_syntax_with_substitutions(
            syntax,
            root_file,
            SemaTypeRootAuthority::KnownFile(root_file),
            span,
            type_substitutions,
            value_substitutions,
        )
        .map_err(|failure| self.type_syntax_compile_error(failure, span))
    }

    #[cfg(test)]
    pub(crate) fn resolve_type_syntax_for_testing(
        &mut self,
        syntax: &str,
        span: Span,
    ) -> CompileResult<Type> {
        self.resolve_type_syntax_in_scope(syntax, span.file_id, span, None)
    }

    fn resolve_type_syntax_with_substitutions(
        &mut self,
        syntax: &str,
        root_file: FileId,
        root_authority: SemaTypeRootAuthority,
        span: Span,
        type_substitutions: Option<&HashMap<Spur, Type>>,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> Result<Type, crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>> {
        let mut provider = SemaTypeSyntaxProvider {
            sema: self,
            span,
            root_authority,
            resolution_context: SemaTypeResolutionContext::Type,
            type_substitutions,
            value_substitutions,
            observed_type_dependencies: Vec::new(),
        };
        let result = crate::resolve_semantic_type_syntax(&mut provider, &root_file, syntax);
        provider.flush_observed_type_dependencies();
        result
    }

    fn type_syntax_compile_error(
        &self,
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
                let display = parse_type_call_syntax(&syntax)
                    .map(|(call, _)| format!("{call}(...)"))
                    .unwrap_or_else(|| syntax.to_string());
                CompileError::new(ErrorKind::UnknownType(display), span)
            }
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
                ..
            })
            | E::Semantic(F::RuntimeConstructorParameter {
                constructor,
                expected,
                found,
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
                        self.interner.resolve(&parameter)
                    ),
                },
                span,
            ),
            E::ComptimeCallTypeArgument { error, .. } => {
                self.type_syntax_compile_error(*error, span)
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

    fn deferred_value_syntax_compile_error(
        &self,
        failure: crate::SemanticTypeSyntaxError<Infallible, CompileError, FileId, Spur>,
        span: Span,
    ) -> CompileError {
        use crate::SemanticResolutionError as E;
        use crate::SemanticTypeSyntaxFailure as F;

        match failure {
            E::Semantic(F::InvalidConstructorArity {
                constructor,
                expected,
                found,
                ..
            }) => CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "compile-time function '{}' expects {} comptime argument(s), but {} were provided",
                        constructor, expected, found
                    ),
                },
                span,
            ),
            E::Semantic(F::RuntimeConstructorParameter { constructor, .. }) => CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "call '{}' is not a compile-time value; all of its parameters must be comptime",
                        constructor
                    ),
                },
                span,
            ),
            failure => self.type_syntax_compile_error(failure, span),
        }
    }

    pub(crate) fn record_resolved_declaration_type(&mut self, ty: Type) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self
                    .type_pool
                    .struct_metadata(id)
                    .expect("resolved declaration type names a registered struct identity");
                if !def.is_builtin && !def.name.starts_with("__anon_struct_") {
                    self.record_resolved_declaration_type_target(
                        def.file_id,
                        def.name,
                        super::DeclarationTypeDependencyTargetKind::Struct,
                    );
                }
            }
            TypeKind::Enum(id) => {
                let def = self
                    .type_pool
                    .enum_metadata(id)
                    .expect("resolved declaration type names a registered enum identity");
                self.record_resolved_declaration_type_target(
                    def.file_id,
                    def.name,
                    super::DeclarationTypeDependencyTargetKind::Enum,
                );
            }
            TypeKind::Array(id) => {
                self.record_resolved_declaration_type(self.type_pool.array_def(id).0)
            }
            TypeKind::PtrConst(id) => {
                self.record_resolved_declaration_type(self.type_pool.ptr_const_def(id));
            }
            TypeKind::PtrMut(id) => {
                self.record_resolved_declaration_type(self.type_pool.ptr_mut_def(id));
            }
            _ => {}
        }
    }

    fn record_resolved_declaration_type_target(
        &mut self,
        target_file: FileId,
        target_name: String,
        target_kind: super::DeclarationTypeDependencyTargetKind,
    ) {
        let Some((source_file, source_name, source_owner_name, source_kind, dependency_kind)) =
            self.declaration_type_observer.clone()
        else {
            return;
        };
        let event = super::DeclarationTypeDependencyEvent {
            source_token: self
                .body_dependency_observer
                .as_ref()
                .and_then(super::AnalyzedBodyOwnerEvent::token),
            source_file: source_file.index(),
            source_name,
            source_owner_name,
            source_kind,
            dependency_kind,
            target_file: target_file.index(),
            target_name,
            target_kind,
        };
        if self.declaration_type_dependencies.contains(&event) {
            return;
        }
        self.declaration_type_dependencies.push(event);
        self.body_analysis_work.declaration_type_dependency_events += 1;
    }

    /// Resolve a type-annotation symbol to a `Type`, consulting the analysis
    /// context's local comptime type bindings at every level of a composite
    /// annotation (RUE-263).
    ///
    /// [`resolve_type`] takes no context, so it can validate a *scalar*
    /// comptime-type-var annotation (`let p: P`, which the caller special-cases
    /// before ever reaching resolution) but not a *composite* one: `[P; 2]` or
    /// `ptr const P` misses in the local-binding map as a whole symbol, and the
    /// context-free recursion then can't resolve the inner `P` (it isn't a named
    /// struct/enum), yielding a spurious E0204. This variant threads
    /// `ctx.comptime_type_vars` through the recursion so an inner
    /// element/pointee that is a local comptime type var resolves. Non-composite
    /// leaves and genuinely-unknown names fall through to [`resolve_type`],
    /// keeping the primitive/struct/enum resolution, privacy checks (E0460), and
    /// the E0204 for a truly unknown type in one place so the two paths can't
    /// drift.
    ///
    /// Array lengths are resolved through `ctx.comptime_value_vars` (RUE-271):
    /// a length naming an enclosing `comptime N: i32` value parameter (`[i32; N]`
    /// / `[P; N]`) binds to its concrete value at each specialization, then
    /// falls back to file-level `const`s and literals like [`resolve_type`]. A
    /// length that resolves to none of these (a runtime parameter) still gets a
    /// clean E0481. The specialized array type is interned into the same pool
    /// the CFG builder later sees (the pool is transferred *after* specialization,
    /// RUE-282), so drop analysis of a comptime-value-length local array no
    /// longer hits an out-of-bounds `ArrayTypeId`.
    ///
    /// [`resolve_type`]: Sema::resolve_type
    pub(crate) fn resolve_type_with_ctx(
        &mut self,
        type_sym: Spur,
        span: Span,
        ctx: &AnalysisContext,
    ) -> CompileResult<Type> {
        let syntax = self.interner.resolve(&type_sym).to_string();
        self.resolve_type_syntax_in_scope(&syntax, span.file_id, span, Some(ctx))
    }

    /// Like [`Self::resolve_qualified_type_name`], but with the file whose
    /// imports anchor the path's first segment made explicit. The comptime
    /// engine resolves a member-access type path (`std.strbuf.StrBuf` as a
    /// value-position type-constructor argument) against its environment's
    /// `defining_file`, the same file the qualified type-annotation path walks
    /// (RUE-948, mirroring the RUE-511/RUE-609 receiver walk).
    pub(crate) fn resolve_qualified_type_name_in_file(
        &mut self,
        root_file: FileId,
        segments: &[&str],
        span: Span,
    ) -> CompileResult<Type> {
        self.resolve_type_syntax_in_scope(&segments.join("."), root_file, span, None)
    }

    /// Like [`Self::resolve_type_module_prefix`], but with the file whose
    /// imports anchor the walk's first segment made explicit. The comptime
    /// engine resolves against its environment's `defining_file` (RUE-511),
    /// which is authoritative even when the expression's span is a default
    /// span with no file context (RUE-609).
    pub(crate) fn resolve_type_module_prefix_in_file(
        &mut self,
        root_file: FileId,
        segments: &[&str],
        span: Span,
    ) -> CompileResult<(crate::types::ModuleId, Option<FileId>, String)> {
        let resolved = crate::resolve_semantic_module_path(
            &mut SemaTypeSyntaxProvider {
                sema: self,
                span,
                root_authority: SemaTypeRootAuthority::KnownFile(root_file),
                resolution_context: SemaTypeResolutionContext::Type,
                type_substitutions: None,
                value_substitutions: None,
                observed_type_dependencies: Vec::new(),
            },
            &root_file,
            segments,
        )
        .map_err(|failure| module_path_resolution_compile_error(failure, span))?;

        let module_id = resolved.module;
        let module_def = self.module_registry.get_def(module_id);
        let module_file_id = Some(module_def.file_id);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or_else(|| module_def.file_path.clone());
        Ok((module_id, module_file_id, module_file_path))
    }

    /// Look up a module binding in the declaration namespace.
    ///
    /// While declaration payloads are being bound, a qualified type may name
    /// an import whose constant appears later in source order. Resolve that
    /// dependency through the declaration index, exactly like any other
    /// constant dependency. Once declaration binding completes, the table is
    /// authoritative: body analysis never rediscovers imports or mutates the
    /// source-declaration namespace after the [`super::BoundSema`] boundary.
    pub(crate) fn resolve_module_binding_in_file(
        &mut self,
        file_id: FileId,
        name: Spur,
    ) -> CompileResult<Option<super::info::ConstInfo>> {
        if let Some(binding) = self.module_binding(&(file_id, name)) {
            return Ok(Some(binding.clone()));
        }
        if !self.declaration_binding_active {
            return Ok(None);
        }

        self.try_resolve_indexed_const_during_binding(name, file_id)?;
        Ok(self.module_binding(&(file_id, name)).cloned())
    }

    /// Return one flag per source parameter identifying declarations of the
    /// form `comptime T: type`.
    ///
    /// This kind cannot be reconstructed from the semantic parameter type:
    /// both a type parameter and a deferred comptime value parameter such as
    /// `comptime value: T` carry [`Type::COMPTIME_TYPE`] until substitution.
    /// The original RIR declaration is therefore the source of truth for
    /// specialization-stream routing and runtime erasure.
    pub(crate) fn comptime_type_param_flags(&self, function: &FunctionInfo) -> Vec<bool> {
        let type_sym = self.interner.get("type");
        let flags: Vec<bool> = self
            .rir
            .params(function.rir_params(self.rir))
            .iter()
            .map(|param| param.is_comptime && Some(param.ty) == type_sym)
            .collect();
        debug_assert_eq!(flags.len(), function.params.len());
        flags
    }

    /// Resolve one parameter of a generic function under a concrete
    /// specialization.
    ///
    /// Declaration gathering uses [`Type::COMPTIME_TYPE`] as a placeholder
    /// for runtime parameter types that mention comptime type/value
    /// parameters (`x: T`, `a: [T; N]`, and so on). Both the call site and the
    /// specialized callee must replace that placeholder through this exact
    /// path. Retaining it after the substitution is known would let them
    /// independently classify the parameter and silently disagree.
    pub(crate) fn resolve_substituted_param_type(
        &mut self,
        function: &FunctionInfo,
        param_index: usize,
        declared: Type,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Type> {
        if declared != Type::COMPTIME_TYPE {
            return Ok(declared);
        }

        let rir_params = self.rir.params(function.rir_params(self.rir));
        let param = rir_params.get(param_index).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "generic parameter index {} is missing from its RIR declaration",
                    param_index
                )),
                function.span,
            )
        })?;
        let type_sym = param.ty;

        self.resolve_substituted_signature_type(type_sym, type_subst, value_subst, function.span)
    }

    /// Resolve the return half of the same specialized signature contract.
    /// Caller and callee must agree on it just as strictly as on parameter
    /// widths; silently retaining the placeholder (or substituting unit) would
    /// merely move the disagreement to the returned value.
    pub(crate) fn resolve_substituted_return_type(
        &mut self,
        function: &FunctionInfo,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<Type> {
        if function.return_type != Type::COMPTIME_TYPE {
            return Ok(function.return_type);
        }
        self.resolve_substituted_signature_type(
            function.return_type_sym,
            type_subst,
            value_subst,
            function.span,
        )
    }

    fn resolve_substituted_signature_type(
        &mut self,
        type_sym: Spur,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Type> {
        let type_name = self.interner.resolve(&type_sym).to_string();
        self.resolve_type_syntax_with_substitutions(
            &type_name,
            span.file_id,
            SemaTypeRootAuthority::KnownFile(span.file_id),
            span,
            Some(type_subst),
            Some(value_subst),
        )
        .map_err(|failure| self.type_syntax_compile_error(failure, span))
    }

    /// Resolve a type symbol to a Type, returning None if the type is unknown.
    ///
    /// This is used in comptime evaluation where we can't produce a compile error.
    pub(crate) fn resolve_type_for_comptime(&mut self, type_sym: Spur) -> Option<Type> {
        self.resolve_type_for_comptime_with_subst(type_sym, &std::collections::HashMap::new())
    }

    /// Resolve a type symbol to a Type with type parameter substitution.
    ///
    /// This is used in comptime evaluation of generic functions where type parameters
    /// need to be substituted with their concrete types. For example, when evaluating
    /// `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }` with T=i32,
    /// we need to resolve `T` to `i32`.
    pub(crate) fn resolve_type_for_comptime_with_subst(
        &mut self,
        type_sym: Spur,
        type_subst: &std::collections::HashMap<Spur, Type>,
    ) -> Option<Type> {
        self.resolve_type_for_comptime_with_subst_and_values(type_sym, type_subst, &HashMap::new())
    }

    /// Resolve a type symbol to a Type with both type-parameter and
    /// value-parameter substitution.
    ///
    /// Like [`resolve_type_for_comptime_with_subst`], but additionally threads
    /// a `comptime` value substitution map so that an array length referring to
    /// a `comptime N: i32` parameter (`[i32; N]`) resolves to its concrete
    /// value at each specialization (RUE-16). File-level `const` lengths are
    /// resolved directly from the constant table and need no substitution.
    ///
    /// [`resolve_type_for_comptime_with_subst`]:
    /// Sema::resolve_type_for_comptime_with_subst
    pub(crate) fn resolve_type_for_comptime_with_subst_and_values(
        &mut self,
        type_sym: Spur,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> Option<Type> {
        let syntax = self.interner.resolve(&type_sym).to_string();
        self.resolve_type_syntax_with_substitutions(
            &syntax,
            FileId::DEFAULT,
            SemaTypeRootAuthority::GlobalSpeculative,
            Span::default(),
            Some(type_subst),
            Some(value_subst),
        )
        .ok()
    }

    pub(crate) fn resolve_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        type_sym: Spur,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<Type> {
        let syntax = self.interner.resolve(&type_sym).to_string();
        self.resolve_type_syntax_with_substitutions(
            &syntax,
            span.file_id,
            SemaTypeRootAuthority::KnownFile(span.file_id),
            span,
            Some(type_subst),
            Some(value_subst),
        )
        .ok()
    }

    fn record_declaration_builtin_type_call_head(&mut self, builtin: super::BuiltinTypeCallHead) {
        let Some((source_file, source_name, source_owner_name, source_kind, dependency_kind)) =
            self.declaration_type_observer.clone()
        else {
            return;
        };
        self.declaration_builtin_type_call_head_dependencies.push(
            super::DeclarationBuiltinTypeCallHeadDependencyEvent {
                source_token: self
                    .body_dependency_observer
                    .as_ref()
                    .and_then(super::AnalyzedBodyOwnerEvent::token),
                source_file: source_file.index(),
                source_name,
                source_owner_name,
                source_kind,
                dependency_kind,
                builtin,
            },
        );
        self.body_analysis_work
            .declaration_type_call_head_dependency_events += 1;
    }

    /// Resolve the capacity argument `N` of a fixed-capacity string `Str(N)`
    /// (ADR-0043 Phase 5, RUE-326) to a concrete `u64`. The argument arrives as
    /// the raw substring of the interned type name: an integer literal
    /// (`Str(8)`) or a `const` name (`Str(CAP)`). Both routes reuse the array
    /// length machinery, so `Str(N)` and `[u8; N]` accept exactly the same
    /// length forms.
    /// Resolve a file-scoped array length through the same provider and shared
    /// comptime-call policy used by canonical type syntax resolution.
    pub(crate) fn resolve_array_length(
        &mut self,
        length: &ArrayLen,
        span: Span,
        value_substitutions: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        let mut provider = SemaTypeSyntaxProvider {
            sema: self,
            span,
            root_authority: SemaTypeRootAuthority::KnownFile(span.file_id),
            resolution_context: SemaTypeResolutionContext::Type,
            type_substitutions: None,
            value_substitutions,
            observed_type_dependencies: Vec::new(),
        };
        let result = provider.resolve_array_length_fact(span.file_id, length);
        provider.flush_observed_type_dependencies();
        result
    }
    /// Check whether a signature type symbol mentions any of the given comptime
    /// type parameters, looking through composite syntax: `[T; N]`,
    /// `ptr const T`, `ptr mut T`, and nestings thereof (RUE-172).
    ///
    /// Used when collecting a generic function's signature to decide whether a
    /// parameter/return type must be deferred (as `Type::COMPTIME_TYPE`) until
    /// specialization instead of resolved eagerly.
    pub(crate) fn type_mentions_type_param(&self, type_sym: Spur, type_params: &[Spur]) -> bool {
        if type_params.contains(&type_sym) {
            return true;
        }
        self.type_name_mentions_type_param(self.interner.resolve(&type_sym), type_params)
    }

    fn type_name_mentions_type_param(&self, type_name: &str, type_params: &[Spur]) -> bool {
        if let Some((element_type, length)) = parse_array_type_syntax(type_name) {
            let length_mentions = match length {
                ArrayLen::Literal(_) => false,
                ArrayLen::Named(name) => self.type_name_mentions_type_param(&name, type_params),
            };
            return length_mentions
                || self.type_name_mentions_type_param(&element_type, type_params);
        }
        if let Some(pointee) = type_name
            .strip_prefix("ptr const ")
            .or_else(|| type_name.strip_prefix("ptr mut "))
        {
            return self.type_name_mentions_type_param(pointee, type_params);
        }
        // A type-function application `Name(arg, ...)` mentions a type parameter
        // if any argument does — `Option(T)` mentions `T`, so a signature/return
        // type applying an enclosing constructor to a type parameter is deferred
        // (as `Type::COMPTIME_TYPE`) until specialization, exactly like `[T; N]`
        // (RUE-272). The constructor name itself is never a type parameter, so
        // only the arguments are inspected.
        if let Some((_call_name, arg_strs)) = parse_type_call_syntax(type_name) {
            return arg_strs
                .iter()
                .any(|arg| self.type_name_mentions_type_param(arg, type_params));
        }
        // Leaf name: only a match against an already-interned symbol counts
        // (type parameter names are interned by the parser).
        match self.interner.get(type_name) {
            Some(sym) => type_params.contains(&sym),
            None => false,
        }
    }

    /// Check whether a signature type symbol mentions any of the given comptime
    /// *value* parameters in an array-length position, looking through
    /// composite syntax: `[i32; N]`, `[[i32; N]; 3]`, `ptr const [i32; N]`, and
    /// nestings thereof (RUE-16).
    ///
    /// Used alongside [`type_mentions_type_param`] when collecting a generic
    /// function's signature: a runtime parameter whose type mentions a comptime
    /// value parameter (only through an array length; the element/pointee can't
    /// be a value) must be deferred (as `Type::COMPTIME_TYPE`) until
    /// specialization, when the length's concrete value is known.
    ///
    /// [`type_mentions_type_param`]: Sema::type_mentions_type_param
    pub(crate) fn type_mentions_comptime_value_param(
        &self,
        type_sym: Spur,
        value_params: &[Spur],
    ) -> bool {
        if value_params.is_empty() {
            return false;
        }
        self.type_name_mentions_value_param(self.interner.resolve(&type_sym), value_params)
    }

    fn type_name_mentions_value_param(&self, type_name: &str, value_params: &[Spur]) -> bool {
        if let Some((element_type, len)) = parse_array_type_syntax(type_name) {
            let length_mentions = match len {
                ArrayLen::Literal(_) => false,
                ArrayLen::Named(name) => self.type_name_mentions_value_param(&name, value_params),
            };
            // Recurse into the element type (nested arrays / pointers).
            return length_mentions
                || self.type_name_mentions_value_param(&element_type, value_params);
        }
        if let Some(pointee) = type_name
            .strip_prefix("ptr const ")
            .or_else(|| type_name.strip_prefix("ptr mut "))
        {
            return self.type_name_mentions_value_param(pointee, value_params);
        }
        if let Some((_call_name, arg_strs)) = parse_type_call_syntax(type_name) {
            return arg_strs
                .iter()
                .any(|arg| self.type_name_mentions_value_param(arg, value_params));
        }
        self.interner
            .get(type_name)
            .is_some_and(|sym| value_params.contains(&sym))
    }

    /// Validate the non-deferred parts of a signature type that will otherwise
    /// be deferred until generic specialization.
    ///
    /// A composite signature such as `[T; 3]` cannot be resolved at declaration
    /// time because the element type is a comptime type parameter. Its length is
    /// still a declaration-time legality question, though: `[T; A]` must reject
    /// an undefined `A` immediately instead of surviving until specialization
    /// and becoming an ICE (RUE-381). Lengths may be literals, file constants, or
    /// comptime value parameters owned by the same function.
    ///
    /// Non-deferred leaf types are declaration-time legality questions too:
    /// `[i3; N]` mentions the comptime value parameter `N`, so the array type as
    /// a whole is deferred, but the element type `i3` is not deferred and should
    /// be reported as E0204 immediately instead of becoming a failed
    /// specialization substitution later.
    pub(crate) fn validate_deferred_signature_type_lengths(
        &mut self,
        type_sym: Spur,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        span: Span,
    ) -> CompileResult<()> {
        self.validate_deferred_signature_type_name_lengths(
            self.interner.resolve(&type_sym).to_string(),
            type_params,
            value_params,
            value_param_type_syms,
            span,
        )
    }

    fn record_declaration_type_call_head(&mut self, function_key: Spur, info: FunctionInfo) {
        let Some((source_file, source_name, source_owner_name, source_kind, dependency_kind)) =
            self.declaration_type_observer.clone()
        else {
            return;
        };
        self.declaration_type_call_head_dependencies.push(
            super::DeclarationTypeCallHeadDependencyEvent {
                source_token: self
                    .body_dependency_observer
                    .as_ref()
                    .and_then(super::AnalyzedBodyOwnerEvent::token),
                source_file: source_file.index(),
                source_name,
                source_owner_name,
                source_kind,
                dependency_kind,
                callable_file: info.file_id.index(),
                callable_name: self
                    .interner
                    .resolve(&self.source_function_name(function_key))
                    .to_string(),
            },
        );
        self.body_analysis_work
            .declaration_type_call_head_dependency_events += 1;
    }

    /// Whether the declaration's source return annotation is literally `type`.
    ///
    /// `FunctionInfo::return_type` cannot answer this: declaration gathering
    /// also uses `COMPTIME_TYPE` as the placeholder for a dependent return such
    /// as `-> T`. Kind-sensitive call paths must therefore consult the original
    /// source symbol instead of the semantic placeholder.
    pub(crate) fn function_returns_type(&self, function: &FunctionInfo) -> bool {
        self.interner.get("type") == Some(function.return_type_sym)
    }

    fn validate_deferred_signature_type_name_lengths(
        &mut self,
        type_name: String,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        span: Span,
    ) -> CompileResult<()> {
        self.validate_deferred_type_position(
            type_name,
            type_params,
            value_params,
            value_param_type_syms,
            span,
        )
        .map(|_| ())
    }

    /// Validate a source fragment used in type position. The optional result
    /// is the concrete type when the fragment is independent of the enclosing
    /// generic parameters; `None` means specialization must finish it.
    fn validate_deferred_type_position(
        &mut self,
        type_name: String,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        span: Span,
    ) -> CompileResult<Option<Type>> {
        let mut provider = DeferredSemaTypeSyntaxProvider {
            inner: SemaTypeSyntaxProvider {
                sema: self,
                span,
                root_authority: SemaTypeRootAuthority::KnownFile(span.file_id),
                resolution_context: SemaTypeResolutionContext::Type,
                type_substitutions: None,
                value_substitutions: None,
                observed_type_dependencies: Vec::new(),
            },
            type_params,
            value_params,
            value_param_type_syms,
        };
        let result = crate::resolve_semantic_type_syntax(&mut provider, &span.file_id, &type_name);
        provider.inner.flush_observed_type_dependencies();
        match result {
            Ok(DeferredTypeResolution::Resolved(ty)) => Ok(Some(ty)),
            Ok(DeferredTypeResolution::Pending) => Ok(None),
            Err(failure) => Err(self.type_syntax_compile_error(failure, span)),
        }
    }

    #[cfg(test)]
    pub(crate) fn validate_deferred_type_position_for_testing(
        &mut self,
        type_name: &str,
        type_params: &[Spur],
        span: Span,
    ) -> CompileResult<Option<Type>> {
        self.validate_deferred_type_position(type_name.to_string(), type_params, &[], &[], span)
    }

    fn deferred_signature_substitutions_are_ready(
        &self,
        type_name: &str,
        type_params: &[Spur],
        value_params: &[Spur],
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> bool {
        type_params.iter().all(|name| {
            type_subst.contains_key(name)
                || !self.type_name_mentions_type_param(type_name, &[*name])
        }) && value_params.iter().all(|name| {
            value_subst.contains_key(name)
                || !self.type_name_mentions_value_param(type_name, &[*name])
        })
    }

    /// Validate one source fragment in compile-time value position. Concrete
    /// values are returned for partial binding; an enclosing comptime
    /// parameter is valid but remains `None` until specialization.
    fn validate_deferred_value_position(
        &mut self,
        value_name: &str,
        type_params: &[Spur],
        value_params: &[Spur],
        value_param_type_syms: &[(Spur, Spur)],
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        let value_name = value_name.trim();
        if let Some(sym) = self.interner.get(value_name) {
            if type_params.contains(&sym) {
                if expected != Some(Type::COMPTIME_TYPE) && (expected.is_some() || require_integer)
                {
                    if let Some(expected) = expected {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: expected.safe_name_with_pool(Some(&self.type_pool)),
                                found: Type::COMPTIME_TYPE
                                    .safe_name_with_pool(Some(&self.type_pool)),
                            },
                            span,
                        ));
                    }
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "compile-time type parameter '{}' is a type, not an integer value",
                                value_name
                            ),
                        },
                        span,
                    ));
                }
                return Ok(None);
            }
            if value_params.contains(&sym) {
                let found = if let Some(type_sym) = value_param_type_syms
                    .iter()
                    .find_map(|(name, type_sym)| (*name == sym).then_some(*type_sym))
                {
                    let type_name = self.interner.resolve(&type_sym).to_string();
                    let depends_on_outer_param = self
                        .type_name_mentions_type_param(&type_name, type_params)
                        || self.type_name_mentions_value_param(&type_name, value_params);
                    if depends_on_outer_param {
                        None
                    } else {
                        Some(self.resolve_type(type_sym, span)?)
                    }
                } else {
                    None
                };
                self.validate_deferred_value_result(
                    None,
                    found,
                    expected,
                    contract,
                    require_integer,
                    value_name,
                    span,
                )?;
                return Ok(None);
            }
        }

        if let Some((call_name, args)) = parse_type_call_syntax(value_name) {
            let mut provider = DeferredSemaTypeSyntaxProvider {
                inner: SemaTypeSyntaxProvider {
                    sema: self,
                    span,
                    root_authority: SemaTypeRootAuthority::KnownFile(span.file_id),
                    resolution_context: SemaTypeResolutionContext::Type,
                    type_substitutions: None,
                    value_substitutions: None,
                    observed_type_dependencies: Vec::new(),
                },
                type_params,
                value_params,
                value_param_type_syms,
            };
            let call = crate::resolve_semantic_comptime_call(
                &mut provider,
                &span.file_id,
                &call_name,
                &args,
                crate::SemanticComptimeCallExpectation::Value,
            );
            provider.inner.flush_observed_type_dependencies();
            let call =
                call.map_err(|failure| self.deferred_value_syntax_compile_error(failure, span))?;
            let function = *self
                .functions
                .get(&call.head.key)
                .expect("selected deferred call has function metadata");
            let callee_types = call
                .type_arguments
                .iter()
                .filter_map(|(name, value)| match value {
                    DeferredTypeResolution::Resolved(value) => Some((*name, *value)),
                    DeferredTypeResolution::Pending => None,
                })
                .collect::<HashMap<_, _>>();
            let callee_values = call
                .value_arguments
                .iter()
                .filter_map(|(name, value)| match value {
                    DeferredValueResolution::Resolved(value) => Some((*name, *value)),
                    DeferredValueResolution::Pending => None,
                })
                .collect::<HashMap<_, _>>();
            let concrete = match call.result {
                crate::SemanticComptimeCallResult::Type(DeferredTypeResolution::Resolved(ty)) => {
                    Some(ConstValue::Type(ty))
                }
                crate::SemanticComptimeCallResult::Value(DeferredValueResolution::Resolved(
                    value,
                )) => Some(value),
                crate::SemanticComptimeCallResult::Type(DeferredTypeResolution::Pending)
                | crate::SemanticComptimeCallResult::Value(DeferredValueResolution::Pending) => {
                    None
                }
            };
            let found = if call.head.returns_type {
                Some(Type::COMPTIME_TYPE)
            } else if function.return_type != Type::COMPTIME_TYPE {
                Some(function.return_type)
            } else {
                let callee_type_params = call
                    .head
                    .parameters
                    .iter()
                    .filter_map(|parameter| parameter.is_type.then_some(parameter.name))
                    .collect::<Vec<_>>();
                let callee_value_params = call
                    .head
                    .parameters
                    .iter()
                    .filter_map(|parameter| (!parameter.is_type).then_some(parameter.name))
                    .collect::<Vec<_>>();
                let return_name = self.interner.resolve(&function.return_type_sym);
                if self.deferred_signature_substitutions_are_ready(
                    return_name,
                    &callee_type_params,
                    &callee_value_params,
                    &callee_types,
                    &callee_values,
                ) {
                    Some(self.resolve_substituted_return_type(
                        &function,
                        &callee_types,
                        &callee_values,
                    )?)
                } else {
                    None
                }
            };
            self.validate_deferred_value_result(
                concrete,
                found,
                expected,
                contract,
                require_integer,
                value_name,
                span,
            )?;
            return Ok(concrete);
        }

        let value = match (SemaTypeSyntaxProvider {
            sema: self,
            span,
            root_authority: SemaTypeRootAuthority::KnownFile(span.file_id),
            resolution_context: SemaTypeResolutionContext::Type,
            type_substitutions: None,
            value_substitutions: None,
            observed_type_dependencies: Vec::new(),
        })
        .resolve_value_argument_fact(span.file_id, "compile-time call", value_name)
        {
            Ok(value) => value,
            Err(_) if require_integer => {
                // This is the outer array-length boundary, not a nested
                // comptime-call argument. Preserve its established E0481
                // diagnostic for an unknown bare name (`[T; A]`) rather than
                // leaking the generic E1200 value-argument diagnostic.
                let length = value_name
                    .parse::<u64>()
                    .map(ArrayLen::Literal)
                    .unwrap_or_else(|_| ArrayLen::Named(value_name.to_string()));
                ConstValue::Integer(self.resolve_array_length(&length, span, None)? as i128)
            }
            Err(error) => return Err(error),
        };
        self.validate_deferred_value_result(
            Some(value),
            Some(value.get_type()),
            expected,
            contract,
            require_integer,
            value_name,
            span,
        )?;
        Ok(Some(value))
    }

    fn validate_deferred_value_result(
        &self,
        value: Option<ConstValue>,
        found: Option<Type>,
        expected: Option<Type>,
        contract: Option<(Spur, Spur)>,
        require_integer: bool,
        expression: &str,
        span: Span,
    ) -> CompileResult<()> {
        if require_integer {
            if let Some(value) = value {
                let Some(integer) = value.as_int_value() else {
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "array length expression '{}' is not an integer",
                                expression
                            ),
                        },
                        span,
                    ));
                };
                if integer < 0 || u64::try_from(integer).is_err() {
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "array length expression '{}' is outside the valid range",
                                expression
                            ),
                        },
                        span,
                    ));
                }
            } else if let Some(found) = found
                && !found.is_integer()
            {
                return Err(CompileError::new(
                    ErrorKind::InvalidArrayLength {
                        reason: format!(
                            "array length expression '{}' has non-integer type {}",
                            expression,
                            found.safe_name_with_pool(Some(&self.type_pool))
                        ),
                    },
                    span,
                ));
            }
        }

        let Some(expected) = expected else {
            return Ok(());
        };
        if let Some(value) = value {
            let (function_name, param_name) = contract.expect("value contracts name a parameter");
            return self.validate_comptime_value_for_type(
                function_name,
                param_name,
                value,
                expected,
                span,
            );
        }
        if let Some(found) = found
            && found != expected
            && !(found.is_integer() && expected.is_integer())
        {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: expected.safe_name_with_pool(Some(&self.type_pool)),
                    found: found.safe_name_with_pool(Some(&self.type_pool)),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Get or create an array type for the given element type and length.
    pub(crate) fn get_or_create_array_type(
        &mut self,
        element_type: Type,
        length: u64,
    ) -> ArrayTypeId {
        self.type_pool.intern_array_from_type(element_type, length)
    }

    /// Pre-create array types from a resolved InferType.
    ///
    /// This walks the InferType recursively and ensures all array types that will
    /// be needed during `infer_type_to_type` conversion are created beforehand.
    /// This separation enables future parallelization of function analysis, where
    /// all mutations happen in this pre-collection phase.
    pub(crate) fn pre_create_array_types_from_infer_type(&mut self, ty: &InferType) {
        match ty {
            InferType::Array { element, length } => {
                // First recursively process nested array types (e.g., [[i32; 3]; 4])
                self.pre_create_array_types_from_infer_type(element);

                // Convert the element type to get the concrete Type
                // (This is safe because we processed nested arrays first)
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                // Skip `<error>`, comptime-only `type`, and module elements:
                // none can be interned. A `[type; N]` array is diagnosed as
                // E1200 and a `[module; N]` array as E0206 in sema instead of
                // panicking here (RUE-253, RUE-265).
                if elem_ty != Type::ERROR && !Self::is_non_internable_element(elem_ty) {
                    // Pre-create this array type
                    self.get_or_create_array_type(elem_ty, *length);
                }
            }
            InferType::Concrete(_) | InferType::Var(_) | InferType::IntLiteral => {
                // Non-array types don't need pre-creation
            }
        }
    }

    /// Convert an InferType to a concrete Type for use as an array element key.
    ///
    /// This is a helper for `pre_create_array_types_from_infer_type` that converts
    /// the element type without mutating `self.array_types` (since we're in a
    /// pre-creation context where the array type may not exist yet).
    pub(crate) fn infer_type_to_concrete_type_for_key(&self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default
            InferType::Array { element, length } => {
                // For nested arrays, look up or create the array type
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                // A comptime-only `type` or module element (or `<error>`)
                // cannot be interned; propagate `<error>` so the enclosing array
                // is diagnosed as E1200 / E0206 in sema rather than panicking
                // (RUE-253, RUE-265).
                if elem_ty == Type::ERROR || Self::is_non_internable_element(elem_ty) {
                    return Type::ERROR;
                }
                // Get or create the array type in the pool
                let id = self.type_pool.intern_array_from_type(elem_ty, *length);
                Type::new_array(id)
            }
        }
    }

    /// Reject a type whose layout exceeds the implementation's maximum object
    /// size (Appendix C practical limit, RUE-561), returning the slot count on
    /// success. Call this wherever a value of `ty` is MATERIALIZED — a local
    /// or temporary slot allocation, a by-value parameter, `@size_of` /
    /// `@align_of` — so the saturating fallback in [`Self::abi_slot_count`]
    /// is never observable.
    pub(crate) fn require_layout_slots(&self, ty: Type, span: Span) -> CompileResult<u32> {
        match self.checked_abi_slot_count(ty) {
            Some(slots) => Ok(slots as u32),
            None => Err(CompileError::new(
                ErrorKind::TypeTooLarge {
                    type_name: ty.safe_name_with_pool(Some(&self.type_pool)),
                    max_bytes: MAX_TYPE_SIZE_BYTES,
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
                let (element_type, length) = self.type_pool.array_def(array_type_id);
                let element_slots = self.checked_abi_slot_count(element_type)?;
                element_slots.checked_mul(length)?
            }
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                let mut total = 0u64;
                for f in &struct_def.fields {
                    total = total.checked_add(self.checked_abi_slot_count(f.ty)?)?;
                }
                total
            }
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
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
        self.type_pool.provisional_abi_slot_count(ty)
    }
}
