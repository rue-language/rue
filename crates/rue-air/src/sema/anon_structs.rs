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

use crate::sema::context::ConstValue;
use crate::types::{EnumDef, StructDef, StructField, Type};

use super::info::AnonMethodSig;
use super::{DeclarationPhase, Sema};

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

impl<D: DeclarationPhase> Sema<'_, D> {
    pub(crate) fn anonymous_key_cmp(
        left: &IssuedAnonymousNominalKey,
        right: &IssuedAnonymousNominalKey,
    ) -> Ordering {
        fn type_root(value: &IssuedTypeInstanceKey) -> Option<crate::SemanticDefinitionToken> {
            use crate::{NominalInstanceKey as N, TypeInstanceKey as T};
            match value {
                T::Nominal(N::Named(value)) => Some(*value),
                T::Nominal(N::Anonymous(value)) => producer_root(&value.producer),
                T::Array { element, .. } | T::PtrConst(element) | T::PtrMut(element) => {
                    type_root(element)
                }
                _ => None,
            }
        }
        fn function_root(
            value: &IssuedFunctionInstanceKey,
        ) -> Option<crate::SemanticDefinitionToken> {
            use crate::FunctionInstanceKey as F;
            match value {
                F::Definition(value) => Some(*value),
                F::Specialization { base, .. } => function_root(base),
                F::AnonymousMember { owner, .. } | F::DropGlue(owner) => type_root(owner),
            }
        }
        fn producer_root(value: &IssuedStableProducerId) -> Option<crate::SemanticDefinitionToken> {
            match value {
                crate::StableProducerId::Definition(value) => Some(*value),
                crate::StableProducerId::Function(value) => function_root(value),
            }
        }

        producer_root(&left.producer)
            .cmp(&producer_root(&right.producer))
            .then_with(|| left.cmp(right))
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
                            name: std::sync::Arc::from(def.name.as_str()),
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
                        .any(|builtin| builtin.name == def.name)
                    {
                        T::BuiltinNominal {
                            kind: K::Enum,
                            name: std::sync::Arc::from(def.name.as_str()),
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
    ) -> (Type, bool) {
        if let Some(struct_id) = self.anon_struct_identities.get(&identity) {
            return (Type::new_struct(*struct_id), false);
        }

        // Producer-nominal: a key that has not been seen mints its own entity.
        // Create a new one using ID reservation. This avoids the fragile
        // two-phase naming where a temp name is replaced.
        let struct_id = self.type_pool.reserve_struct_id();

        // Now we know the ID, so we can create the final name directly
        let name = format!("__anon_struct_{}", struct_id.0);
        let name_spur = self.interner.get_or_intern(&name);

        // A `drop fn(self)` inside the struct body is carried as a method under
        // the reserved `__drop` name (RUE-312). Its presence means this struct
        // has a user destructor: register `{name}.__drop` as the destructor so
        // the CFG drop glue runs it at scope exit, and force the struct
        // non-Copy (a type with a destructor cannot be `@copy` — the spirit of
        // the named-struct E0457 check).
        let drop_marker = self.interner.get_or_intern("__drop");
        let has_destructor = method_sigs.iter().any(|sig| sig.name == drop_marker);

        // Determine if the struct is Copy (all fields are Copy, and there is no
        // destructor).
        let is_copy =
            !has_destructor && fields.iter().all(|f| f.ty.is_copy_in_pool(&self.type_pool));

        let destructor = if has_destructor {
            Some(format!("{}.__drop", name))
        } else {
            None
        };

        let struct_def = StructDef {
            name,
            fields: fields.to_vec(),
            is_copy,
            is_linear: false,
            destructor,
            is_builtin: false,
            is_pub: false,                     // Anonymous structs are private
            file_id: rue_span::FileId::new(0), // Anonymous, no source file
        };

        // Complete the registration with the final name
        self.type_pool
            .complete_struct_registration(struct_id, name_spur, struct_def);

        // Store method signatures for future structural equality checks
        if !method_sigs.is_empty() {
            self.anon_struct_method_sigs
                .insert(struct_id, method_sigs.to_vec());
        }

        // Store captured comptime values for future structural equality checks and method analysis
        if !captured_values.is_empty() {
            self.anon_struct_captured_values
                .insert(struct_id, captured_values.clone());
        }

        // Register in struct lookup
        self.generated_structs.insert(name_spur, struct_id);
        self.anonymous_struct_ids.insert(struct_id);
        let ty = Type::new_struct(struct_id);
        self.anon_struct_identities
            .insert(identity.clone(), struct_id);
        self.canonical_anonymous_types.insert(ty, identity.clone());
        self.canonical_anonymous_aliases
            .entry(ty)
            .or_default()
            .insert(identity);

        // Return with is_new=true
        (Type::new_struct(struct_id), true)
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
    ) -> Type {
        if let Some(enum_id) = self.anon_enum_identities.get(&identity) {
            return Type::new_enum(*enum_id);
        }

        // Names are presentation/lookup handles only. Source anonymous enums
        // receive a unique live name because producer-distinct identities must
        // not be collapsed by the pool's name interning.
        let mut name = format!("__anon_enum_{} {{ ", self.anon_enum_identities.len());
        for (i, vname) in variant_names.iter().enumerate() {
            if i > 0 {
                name.push_str(", ");
            }
            name.push_str(vname);
            let payload = &variant_payloads[i];
            if !payload.is_empty() {
                name.push('(');
                for (j, ty) in payload.iter().enumerate() {
                    if j > 0 {
                        name.push_str(", ");
                    }
                    name.push_str(&ty.safe_name_with_pool(Some(&self.type_pool)));
                }
                name.push(')');
            }
        }
        name.push_str(" }");

        let name_spur = self.interner.get_or_intern(&name);

        let def = EnumDef {
            name,
            variants: variant_names.to_vec(),
            variant_payloads: variant_payloads.to_vec(),
            is_pub: false,                     // Anonymous enums are private
            file_id: rue_span::FileId::new(0), // Anonymous, no source file
        };

        // `register_enum` dedups by interned name, so an equivalent anonymous
        // enum interned earlier is reused.
        let (enum_id, _is_new) = self.type_pool.register_enum(name_spur, def);

        // Mirror `find_or_create_anon_struct`, which records the type in the
        // name→id lookup so later resolution paths see it.
        self.generated_enums.insert(name_spur, enum_id);
        self.anonymous_enum_ids.insert(enum_id);
        let ty = Type::new_enum(enum_id);
        self.anon_enum_identities.insert(identity.clone(), enum_id);
        self.canonical_anonymous_types.insert(ty, identity.clone());
        self.canonical_anonymous_aliases
            .entry(ty)
            .or_default()
            .insert(identity);

        Type::new_enum(enum_id)
    }

    /// Find the canonical-key-minimum anonymous enum compatible with a
    /// compiler-synthesized use site. This path is lookup-only: a source-less
    /// intrinsic may consume an existing §4.14-compatible type, but it cannot
    /// allocate an entity without a stable producer and structural anchor.
    pub(crate) fn find_compatible_anon_enum(
        &self,
        variant_names: &[String],
        variant_payloads: &[Vec<Type>],
    ) -> Option<Type> {
        self.anon_enum_identities
            .iter()
            .filter(|(_, id)| {
                let def = self.type_pool.enum_def(**id);
                def.variants == variant_names
                    && def.variant_payloads.len() == variant_payloads.len()
                    && def
                        .variant_payloads
                        .iter()
                        .zip(variant_payloads)
                        .all(|(left, right)| {
                            left.len() == right.len()
                                && left
                                    .iter()
                                    .zip(right)
                                    .all(|(left, right)| self.types_equivalent(*left, *right))
                        })
            })
            .min_by(|(left, _), (right, _)| Self::anonymous_key_cmp(left, right))
            .map(|(_, id)| Type::new_enum(*id))
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

    pub(crate) fn types_compatible(&self, found: Type, expected: Type) -> bool {
        found.is_never() || found.is_error() || self.types_equivalent(found, expected)
    }
}
