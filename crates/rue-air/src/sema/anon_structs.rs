//! Anonymous struct handling.
//!
//! Anonymous `struct` and `enum` declaration expressions are *producer-nominal*
//! (ADR-0066). Their identity is the selected declaration expression under its
//! static enclosing comptime specialization — the `AnonymousNominalKey` — not a
//! structural comparison of fields, variants, method signatures, or bodies. Two
//! distinct producers that declare the same shape are distinct, non-assignable
//! types. There is no cross-producer structural search or stable-minimum
//! representative: each producer key owns exactly one entity. Anchor variants of
//! the *same* producer (a body reached under different definition-relative
//! anchor prefixes) alias to that one entity, which is all the alias map now
//! retains.
//!
//! It also implements the enum analog (`find_or_create_anon_enum`): anonymous
//! enum types produced by comptime type functions like
//! `fn Option(comptime T: type) -> type { enum { Some(T), None } }`
//! (ADR-0038, RUE-6 phase 2).

use std::cmp::Ordering;
use std::collections::HashMap;

use lasso::Spur;
use rue_error::CompileResult;

use crate::sema::context::ConstValue;
use crate::types::{StructField, Type};

use super::info::AnonMethodSig;
use super::{DeclarationPhase, Sema};

/// Canonical logical paths of the trusted standard-library `Option`/`Result`
/// modules. The leading NUL is disjoint from every project-relative identity, so
/// a user module can never spell one (RUE-1112). A module resolved under the
/// captured std root — whether pulled by a user `@import` or compiler-rooted for
/// a freestanding fallible intrinsic — is classified onto exactly these paths.
const TRUSTED_OPTION_MODULE_PATH: &str = "\0rue-std/option.rue";
const TRUSTED_RESULT_MODULE_PATH: &str = "\0rue-std/result.rue";

/// The trusted standard-library producer family a `?` operand or enclosing
/// return type is an exact specialization of (RUE-1112).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustedTryProducer {
    Option,
    Result,
}

pub(crate) type IssuedAnonymousNominalKey =
    crate::AnonymousNominalKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedFunctionInstanceKey =
    crate::FunctionInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedTypeInstanceKey =
    crate::TypeInstanceKey<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedCanonicalArguments =
    crate::CanonicalArguments<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

pub(crate) type IssuedStableProducerId =
    crate::StableProducerId<crate::SemanticDefinitionToken, crate::SemanticModuleToken>;

/// Type-distinct, epoch-local producer for a const whose initializer has not
/// yet been classified. It has no stable/durable export conversion.
pub(crate) struct EpochLocalConstCandidateProducer(super::EpochLocalConstCandidateToken);

impl EpochLocalConstCandidateProducer {
    pub(crate) fn into_comptime_producer(self) -> IssuedStableProducerId {
        IssuedStableProducerId::Definition(self.0.0)
    }
}

/// The root source definition a producer-nominal key ultimately derives from,
/// unwinding function specializations, anonymous members, and drop glue down to
/// the owning definition token. `None` for a producer with no source definition
/// root (a builtin or primitive owner).
///
/// This is the single place the AIR domain walks a `StableProducerId` to its
/// definition. It backs both the deterministic export ordering
/// ([`anonymous_key_cmp`]) and trusted-producer recognition under `?`
/// (RUE-1112): a `Type` whose anonymous key roots at the trusted
/// `std/option.rue::Option` (or `std/result.rue::Result`) function definition is
/// an exact std producer, everything else a lookalike.
pub(crate) fn anonymous_producer_root(
    producer: &IssuedStableProducerId,
) -> Option<crate::SemanticDefinitionToken> {
    fn type_root(value: &IssuedTypeInstanceKey) -> Option<crate::SemanticDefinitionToken> {
        use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
        match value {
            T::Nominal(N::Named(value)) => Some(*value),
            T::Nominal(N::Anonymous(value)) => anonymous_producer_root(&value.producer),
            T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                type_root(element)
            }
            _ => None,
        }
    }
    fn function_root(value: &IssuedFunctionInstanceKey) -> Option<crate::SemanticDefinitionToken> {
        use crate::FunctionInstanceKey as F;
        match value {
            F::Definition(value) => Some(*value),
            F::Specialization { base, .. } => function_root(base),
            F::AnonymousMember { owner, .. } | F::DropGlue(owner) => type_root(owner),
        }
    }
    match producer {
        crate::StableProducerId::Definition(value) => Some(*value),
        crate::StableProducerId::Function(value) => function_root(value),
    }
}

/// Deterministic export/presentation ordering of two producer-nominal keys.
///
/// This is a *presentation* order only — it decides how anonymous exports are
/// listed and sorted (see the `provider_body_host.rs` sort), never type identity.
/// Identity is the producer-nominal `AnonymousNominalKey` itself (ADR-0066);
/// two keys that order equal here are still distinct types unless they are the
/// same key. Since RUE-1112 deleted the last min-selection consumer
/// (`find_compatible_anon_enum`), no code treats this ordering as identity
/// authority.
pub(crate) fn anonymous_key_cmp(
    left: &IssuedAnonymousNominalKey,
    right: &IssuedAnonymousNominalKey,
) -> Ordering {
    anonymous_producer_root(&left.producer)
        .cmp(&anonymous_producer_root(&right.producer))
        .then_with(|| left.cmp(right))
}

impl<D: DeclarationPhase> Sema<'_, D> {
    /// The trusted standard-library producer family `ty` is an exact
    /// specialization of, or `None` for a non-enum or a lookalike (RUE-1112).
    ///
    /// `?` legality is exact-producer identity, not shape: an enum is a trusted
    /// `Option`/`Result` only when its producer-nominal key roots at the trusted
    /// `std/option.rue::Option` / `std/result.rue::Result` function definition.
    /// Recognition compares the enum's producer key — obtained through the
    /// `Type` -> `AnonymousNominalKey` map — against that well-known trusted
    /// identity, reading the producer's endpoint (module logical path + name +
    /// kind). It never loads or materializes std `Result`/`Option` to reject a
    /// same-shape lookalike: a lookalike simply roots at a different (user)
    /// definition and is rejected without touching the trusted module.
    pub(crate) fn trusted_try_producer(&self, ty: Type) -> Option<TrustedTryProducer> {
        let enum_id = ty.as_enum()?;
        let identity = self
            .canonical_anonymous_types
            .get(&Type::new_enum(enum_id))?;
        let root = anonymous_producer_root(&identity.producer)?;
        let endpoint = self.stable_definition_endpoints.get(&root)?;
        if endpoint.owner.is_some() || endpoint.kind != crate::StableDefinitionKind::Function {
            return None;
        }
        match (
            self.stable_logical_module_component(endpoint.file),
            endpoint.name.as_ref(),
        ) {
            (TRUSTED_OPTION_MODULE_PATH, "Option") => Some(TrustedTryProducer::Option),
            (TRUSTED_RESULT_MODULE_PATH, "Result") => Some(TrustedTryProducer::Result),
            _ => None,
        }
    }

    pub(crate) fn canonical_definition_producer(
        &self,
        file: rue_span::FileId,
        name: &str,
        owner: Option<&str>,
        kind: crate::StableDefinitionKind,
    ) -> Result<IssuedStableProducerId, crate::SemanticBodyExportFailure> {
        Ok(IssuedStableProducerId::Definition(
            self.stable_definition_token(file.index(), name, owner, kind)?,
        ))
    }

    pub(crate) fn epoch_local_const_candidate_producer(
        &self,
        file: rue_span::FileId,
        name: &str,
    ) -> Result<EpochLocalConstCandidateProducer, crate::SemanticBodyExportFailure> {
        self.const_candidate_tokens
            .get(&(file.index(), name.to_owned()))
            .copied()
            .map(EpochLocalConstCandidateProducer)
            .ok_or(crate::SemanticBodyExportFailure::MissingStableIdentity)
    }

    pub(crate) fn canonical_type_instance(
        &self,
        ty: Type,
    ) -> Result<IssuedTypeInstanceKey, crate::SemanticBodyExportFailure> {
        use crate::SemanticBodyExportFailure as F;
        use crate::{AnonymousNominalKind as K, NominalInstanceKey as N, TypeInstanceKey as T};

        Ok(match ty.kind() {
            crate::TypeKind::I8 => T::I8,
            crate::TypeKind::I16 => T::I16,
            crate::TypeKind::I32 => T::I32,
            crate::TypeKind::I64 => T::I64,
            crate::TypeKind::U8 => T::U8,
            crate::TypeKind::U16 => T::U16,
            crate::TypeKind::U32 => T::U32,
            crate::TypeKind::U64 => T::U64,
            crate::TypeKind::Bool => T::Bool,
            crate::TypeKind::Unit => T::Unit,
            crate::TypeKind::Never => T::Never,
            crate::TypeKind::ComptimeType => T::ComptimeType,
            crate::TypeKind::Struct(id) => {
                if let Some(key) = self.canonical_anonymous_types.get(&ty) {
                    T::Nominal(N::Anonymous(key.clone()))
                } else {
                    // Nominal identity is available from the declaration shell,
                    // before fields are complete. Comptime type constructors can
                    // legitimately receive such a nominal while declarations are
                    // still being collected; identity must not depend on layout.
                    let def = self
                        .type_pool
                        .struct_metadata(id)
                        .ok_or(F::UnsupportedType)?;
                    if def.is_builtin {
                        T::BuiltinNominal {
                            kind: K::Struct,
                            name: def.name.clone(),
                        }
                    } else {
                        T::Nominal(N::Named(self.struct_identity(id)?))
                    }
                }
            }
            crate::TypeKind::Enum(id) => {
                if let Some(key) = self.canonical_anonymous_types.get(&ty) {
                    T::Nominal(N::Anonymous(key.clone()))
                } else {
                    let def = self.type_pool.enum_metadata(id).ok_or(F::UnsupportedType)?;
                    if rue_builtins::BUILTIN_ENUMS
                        .iter()
                        .any(|builtin| builtin.name == &*def.name)
                    {
                        T::BuiltinNominal {
                            kind: K::Enum,
                            name: def.name.clone(),
                        }
                    } else {
                        T::Nominal(N::Named(self.enum_identity(id)?))
                    }
                }
            }
            crate::TypeKind::Array(id) => {
                let (element, len) = self.type_pool.array_def(id);
                T::Array {
                    element: Box::new(self.canonical_type_instance(element)?),
                    len,
                }
            }
            crate::TypeKind::PtrConst(id) => T::PtrConst(Box::new(
                self.canonical_type_instance(self.type_pool.ptr_const_def(id))?,
            )),
            crate::TypeKind::PtrMut(id) => T::PtrMut(Box::new(
                self.canonical_type_instance(self.type_pool.ptr_mut_def(id))?,
            )),
            crate::TypeKind::Module(id) => {
                let file = self.module_registry.get_def(id).file_id;
                T::Module(
                    self.stable_module_tokens
                        .get(&file)
                        .copied()
                        .or_else(|| {
                            self.stable_module_tokens
                                .is_empty()
                                .then(|| crate::SemanticModuleToken::new(0, file.index()))
                        })
                        .ok_or(F::MissingStableIdentity)?,
                )
            }
            crate::TypeKind::Error => return Err(F::UnsupportedType),
        })
    }

    pub(crate) fn canonical_argument_value(
        &self,
        value: ConstValue,
    ) -> Result<
        crate::CanonicalArgumentValue<crate::SemanticDefinitionToken, crate::SemanticModuleToken>,
        crate::SemanticBodyExportFailure,
    > {
        use crate::CanonicalArgumentValue as V;
        Ok(match value {
            ConstValue::Integer(value) => V::Integer(value),
            ConstValue::Bool(value) => V::Bool(value),
            ConstValue::Type(value) => V::Type(Box::new(self.canonical_type_instance(value)?)),
            ConstValue::Function(value) => V::Function(Box::new(
                IssuedFunctionInstanceKey::Definition(self.function_identity(value)?),
            )),
            ConstValue::Unit => V::Unit,
            ConstValue::String(value) => {
                V::String(std::sync::Arc::from(self.interner.resolve(&value)))
            }
        })
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
                .map(|ty| self.canonical_type_instance(*ty))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
            values: value_args
                .iter()
                .copied()
                .map(|value| self.canonical_argument_value(value))
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        };
        Ok(IssuedFunctionInstanceKey::Specialization {
            base: Box::new(IssuedFunctionInstanceKey::Definition(
                self.function_identity(function_name)?,
            )),
            arguments,
        })
    }

    pub(crate) fn canonical_function_producer(
        &self,
        function_name: Spur,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
    ) -> Result<(IssuedStableProducerId, IssuedCanonicalArguments), crate::SemanticBodyExportFailure>
    {
        use crate::SemanticBodyExportFailure as F;
        let function = self
            .function_info(function_name)
            .ok_or(F::MissingStableIdentity)?;
        let type_flags = self.comptime_type_param_flags(function);
        let mut types = Vec::new();
        let mut values = Vec::new();
        for (index, (name, _, _, is_comptime)) in self.param_arena.iter(function.params).enumerate()
        {
            if !*is_comptime {
                continue;
            }
            if type_flags[index] {
                types.push(self.canonical_type_instance(
                    *type_subst.get(name).ok_or(F::MissingStableIdentity)?,
                )?);
            } else {
                values.push(self.canonical_argument_value(
                    *value_subst.get(name).ok_or(F::MissingStableIdentity)?,
                )?);
            }
        }
        let arguments = IssuedCanonicalArguments {
            types: types.into(),
            values: values.into(),
        };
        let base = IssuedFunctionInstanceKey::Definition(self.function_identity(function_name)?);
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

    pub(crate) fn canonical_anonymous_member_producer(
        &self,
        owner: Type,
        name: Spur,
        kind: crate::AnonymousMemberKind,
    ) -> Result<IssuedStableProducerId, crate::SemanticBodyExportFailure> {
        let owner = self.canonical_type_instance(owner)?;
        Ok(IssuedStableProducerId::Function(Box::new(
            IssuedFunctionInstanceKey::AnonymousMember {
                owner: Box::new(owner),
                member: crate::AnonymousMemberKey {
                    kind,
                    name: std::sync::Arc::from(self.interner.resolve(&name)),
                },
            },
        )))
    }
}

impl<D: DeclarationPhase> Sema<'_, D> {
    /// Return the request-independent content of one definition token.
    pub(in crate::sema) fn stable_definition_symbol_component(
        &self,
        token: &crate::SemanticDefinitionToken,
    ) -> String {
        match self.stable_definition_endpoints.get(token) {
            Some(endpoint) => crate::stable_digest::stable_definition_component(
                self.stable_logical_module_component(endpoint.file),
                &endpoint.name,
                endpoint.owner.as_deref(),
                endpoint.kind as u8,
            ),
            None => format!("d\u{1}{}\u{1}{}", token.issuer(), token.slot()),
        }
    }

    /// Return the request-independent logical module content of one module token.
    pub(in crate::sema) fn stable_module_symbol_component(
        &self,
        token: &crate::SemanticModuleToken,
    ) -> String {
        match self.stable_module_endpoints.get(token) {
            Some(endpoint) => crate::stable_digest::stable_module_component(
                self.stable_logical_module_component(endpoint.file),
            ),
            None => format!("m\u{1}{}\u{1}{}", token.issuer(), token.slot()),
        }
    }

    /// Resolve an installed endpoint's request-independent logical module path.
    ///
    /// Installed endpoints without a canonical path violate the sealed request
    /// identity invariant and must fail closed.
    pub(in crate::sema) fn stable_logical_module_component(&self, file: u32) -> &str {
        self.symbol_paths
            .get(&rue_span::FileId::new(file))
            .map(String::as_str)
            .expect("every installed endpoint's file has a canonical logical module path")
    }

    /// Return the producer-nominal anonymous struct for `identity`, creating it
    /// if this producer key has not been materialized yet.
    ///
    /// Identity is producer-nominal (ADR-0066): the `AnonymousNominalKey` alone
    /// owns the entity. There is no structural search across producers and no
    /// stable-minimum representative — two distinct producers with identical
    /// fields and method signatures are distinct types. Method signatures,
    /// captured values, and bodies are *content* of the type, not part of its
    /// identity.
    ///
    /// Returns a tuple of (Type, is_new) where is_new indicates whether the struct was
    /// newly created (true) or an existing match was found (false). Callers should only
    /// register methods for newly created structs.
    pub(crate) fn find_or_create_anon_struct(
        &mut self,
        identity: IssuedAnonymousNominalKey,
        fields: &[StructField],
        method_sigs: &[AnonMethodSig],
        captured_values: &HashMap<Spur, ConstValue>,
    ) -> CompileResult<(Type, bool)> {
        super::ordinary_engine::OrdinaryBodyEngine::new(self).find_or_create_anon_struct(
            identity,
            fields,
            method_sigs,
            captured_values,
        )
    }

    /// Return the producer-nominal anonymous enum for `identity`, creating it
    /// if this producer key has not been materialized yet. The enum analog of
    /// [`Self::find_or_create_anon_struct`].
    ///
    /// Identity is producer-nominal (ADR-0066): the `AnonymousNominalKey` owns
    /// the entity. There is no structural search across producers — two
    /// distinct producers declaring the same variants are distinct types.
    /// Because payload types are already fully monomorphized here, the synthetic
    /// name encodes them (e.g. an `[i32; N]` payload) for presentation, but the
    /// name never decides identity: each producer key receives a fresh live name
    /// so the pool's name interning cannot collapse producer-distinct types.
    pub(crate) fn find_or_create_anon_enum(
        &mut self,
        identity: IssuedAnonymousNominalKey,
        variant_names: &[String],
        variant_payloads: &[Vec<Type>],
    ) -> CompileResult<Type> {
        super::ordinary_engine::OrdinaryBodyEngine::new(self).find_or_create_anon_enum(
            identity,
            variant_names,
            variant_payloads,
        )
    }

    /// Canonical semantic type equivalence.
    ///
    /// Nominal identity is exact for named types and, since ADR-0066, for
    /// anonymous types as well: two anonymous nominals are the same type iff
    /// they carry the same producer-nominal `AnonymousNominalKey`, never because
    /// their fields, variants, method signatures, or bodies coincide. Named,
    /// array, and pointer composition still recurses. This is deliberately
    /// separate from allocation identity and from recovery coercions such as
    /// `never` and `<error>`.
    pub(crate) fn types_equivalent(&self, left: Type, right: Type) -> bool {
        self.types_equivalent_inner(left, right, &mut std::collections::HashSet::new())
    }

    fn types_equivalent_inner(
        &self,
        left: Type,
        right: Type,
        visited: &mut std::collections::HashSet<(Type, Type)>,
    ) -> bool {
        if left == right {
            return true;
        }
        if !visited.insert((left, right)) {
            return true;
        }
        match (left.kind(), right.kind()) {
            (crate::TypeKind::Array(left), crate::TypeKind::Array(right)) => {
                let (left_element, left_len) = self.type_pool.array_def(left);
                let (right_element, right_len) = self.type_pool.array_def(right);
                left_len == right_len
                    && self.types_equivalent_inner(left_element, right_element, visited)
            }
            (crate::TypeKind::PtrConst(left), crate::TypeKind::PtrConst(right)) => self
                .types_equivalent_inner(
                    self.type_pool.ptr_const_def(left),
                    self.type_pool.ptr_const_def(right),
                    visited,
                ),
            (crate::TypeKind::PtrMut(left), crate::TypeKind::PtrMut(right)) => self
                .types_equivalent_inner(
                    self.type_pool.ptr_mut_def(left),
                    self.type_pool.ptr_mut_def(right),
                    visited,
                ),
            (crate::TypeKind::Struct(left_id), crate::TypeKind::Struct(right_id))
                if self.anonymous_struct_ids.contains(&left_id)
                    && self.anonymous_struct_ids.contains(&right_id) =>
            {
                self.anonymous_nominals_have_same_producer(left, right)
            }
            (crate::TypeKind::Enum(left_id), crate::TypeKind::Enum(right_id))
                if self.anonymous_enum_ids.contains(&left_id)
                    && self.anonymous_enum_ids.contains(&right_id) =>
            {
                self.anonymous_nominals_have_same_producer(left, right)
            }
            _ => false,
        }
    }

    /// Producer-nominal identity comparison (ADR-0066): two anonymous nominals
    /// are the same type iff they carry the same `AnonymousNominalKey`. Because
    /// each producer key owns exactly one live entity, equal keys imply the same
    /// `Type` handle (already caught by the `left == right` fast path); this
    /// method makes the producer-nominal rule explicit and fails closed when a
    /// live anonymous type has no recorded identity.
    fn anonymous_nominals_have_same_producer(&self, left: Type, right: Type) -> bool {
        match (
            self.canonical_anonymous_types.get(&left),
            self.canonical_anonymous_types.get(&right),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }
}

#[cfg(test)]
mod theme4b_digest_collision_tests {
    //! RUE-1089 Theme 4b — the anonymous-symbol digest must never decide type
    //! identity. A deterministic exact-key collision registry, shared by the
    //! struct and enum minting paths, rejects any second DISTINCT
    //! `AnonymousNominalKey` that hashes to a digest already owned, so a
    //! `register_enum`/`register_struct` name dedup can never silently collapse
    //! two producer-distinct types. These tests point two distinct keys at one
    //! digest through the test-only forced-digest hook and assert fail-closed
    //! behavior in both insertion orders, plus same-key reuse.

    use lasso::ThreadedRodeo;
    use rue_error::{ErrorCode, PreviewFeatures};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_rir::{AstGen, Rir, RirStructuralAnchor, RirStructuralPathSegment};

    use super::IssuedAnonymousNominalKey;
    use crate::sema::Sema;
    use crate::types::Type;

    const FORCED_DIGEST: u128 = 0x0BAD_C0DE_0BAD_C0DE_0BAD_C0DE_0BAD_C0DE;

    fn lowered_main() -> (Rir, ThreadedRodeo) {
        let (tokens, interner) = Lexer::new("fn main() {}").tokenize().unwrap();
        let (ast, interner) = Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        (astgen.finish(), interner)
    }

    /// Two distinct anonymous-enum producer keys differing only by anchor
    /// segment. Same producer, distinct anchors -> distinct keys.
    fn enum_key(anchor_seg: u32) -> IssuedAnonymousNominalKey {
        crate::AnonymousNominalKey {
            kind: crate::AnonymousNominalKind::Enum,
            producer: crate::StableProducerId::Definition(crate::SemanticDefinitionToken::new(
                7, 0,
            )),
            anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(
                anchor_seg,
            )]),
            arguments: crate::CanonicalArguments::default(),
        }
    }

    fn register(
        sema: &mut Sema<'_>,
        key: IssuedAnonymousNominalKey,
    ) -> rue_error::CompileResult<Type> {
        sema.find_or_create_anon_enum(
            key,
            &["A".to_string(), "B".to_string()],
            &[Vec::new(), Vec::new()],
        )
    }

    /// (a) Distinct keys forced onto one digest: the second fails closed with a
    /// typed E9000, and neither the colliding nominal nor its symbol is published.
    #[test]
    fn distinct_keys_on_one_digest_fail_closed_order_a() {
        let (rir, interner) = lowered_main();
        let mut sema = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new());
        let first = enum_key(0);
        let second = enum_key(1);
        assert_ne!(first, second, "the two producer keys must be distinct");
        sema.forced_anonymous_digests
            .insert(first.clone(), FORCED_DIGEST);
        sema.forced_anonymous_digests
            .insert(second.clone(), FORCED_DIGEST);

        register(&mut sema, first).expect("the first key mints its own entity");
        let published_before = sema.anon_enum_identities.len();

        let error =
            register(&mut sema, second.clone()).expect_err("the colliding key must fail closed");
        assert_eq!(
            error.kind.code(),
            ErrorCode::INTERNAL_ERROR,
            "digest collision must be a typed E9000 internal error",
        );
        // Zero publication: no new enum identity, and the colliding key is absent
        // from the identity cache (so no symbol was minted for it either).
        assert_eq!(
            sema.anon_enum_identities.len(),
            published_before,
            "a colliding key must not publish a nominal",
        );
        assert!(
            !sema.anon_enum_identities.contains_key(&second),
            "the colliding key must not be cached",
        );
    }

    /// (b) The same collision in the reversed insertion order fails closed too —
    /// the registry is order-independent.
    #[test]
    fn distinct_keys_on_one_digest_fail_closed_order_b() {
        let (rir, interner) = lowered_main();
        let mut sema = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new());
        let first = enum_key(0);
        let second = enum_key(1);
        sema.forced_anonymous_digests
            .insert(first.clone(), FORCED_DIGEST);
        sema.forced_anonymous_digests
            .insert(second.clone(), FORCED_DIGEST);

        // Reversed: mint `second` first, then `first` collides.
        register(&mut sema, second).expect("the first-registered key mints its own entity");
        let published_before = sema.anon_enum_identities.len();

        let error =
            register(&mut sema, first.clone()).expect_err("the colliding key must fail closed");
        assert_eq!(error.kind.code(), ErrorCode::INTERNAL_ERROR);
        assert_eq!(sema.anon_enum_identities.len(), published_before);
        assert!(!sema.anon_enum_identities.contains_key(&first));
    }

    /// (c) Re-presenting the SAME key is legitimate reuse, before and after other
    /// registrations — never a collision.
    #[test]
    fn same_key_reuses_before_and_after_other_registrations() {
        let (rir, interner) = lowered_main();
        let mut sema = Sema::new_synthetic(&rir, &interner, PreviewFeatures::new());
        let key = enum_key(0);
        let other = enum_key(5);

        // Reuse BEFORE any other registration.
        let minted = register(&mut sema, key.clone()).expect("first mint");
        let reused_before = register(&mut sema, key.clone()).expect("same key reuses");
        assert_eq!(minted, reused_before, "same key must reuse its entity");

        // An unrelated distinct key (its own natural digest) registers fine.
        register(&mut sema, other).expect("an unrelated key mints its own entity");

        // Reuse AFTER the unrelated registration.
        let reused_after = register(&mut sema, key).expect("same key still reuses");
        assert_eq!(minted, reused_after);
        assert_eq!(
            sema.anon_enum_identities.len(),
            2,
            "exactly the two distinct keys own entities",
        );
    }
}
