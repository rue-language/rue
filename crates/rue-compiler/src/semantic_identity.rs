//! Position- and schedule-independent semantic identities.
//!
//! These values are the durable names of semantic entities.  They deliberately
//! contain only canonical source identities and canonical argument values; live
//! AIR/RIR handles and source locations belong to the request that materializes
//! an entity and must never enter this algebra.

#![allow(dead_code)] // Phase-4 seams consumed incrementally by later query families.

use std::fmt::Write as _;

use crate::{ModuleId, StableDefinitionKey, bound_definitions::StableNamedTypeKey};

pub use rue_air::{
    AnonymousMemberKey, AnonymousMemberKind, AnonymousNominalKind, CompilerCallableId,
    LocalAtomKind,
};
pub use rue_rir::{
    RirStructuralAnchor as StructuralAnchor, RirStructuralPathSegment as StructuralPathSegment,
};

pub type CanonicalArgumentValue = rue_air::CanonicalArgumentValue<StableDefinitionKey, ModuleId>;
pub type CanonicalArguments = rue_air::CanonicalArguments<StableDefinitionKey, ModuleId>;
pub type StableProducerId = rue_air::StableProducerId<StableDefinitionKey, ModuleId>;
pub type AnonymousNominalKey = rue_air::AnonymousNominalKey<StableDefinitionKey, ModuleId>;
pub type NominalInstanceKey = rue_air::NominalInstanceKey<StableDefinitionKey, ModuleId>;
pub type TypeInstanceKey = rue_air::TypeInstanceKey<StableDefinitionKey, ModuleId>;
pub type FunctionInstanceKey = rue_air::FunctionInstanceKey<StableDefinitionKey, ModuleId>;
pub type StableCallableId = rue_air::StableCallableId<StableDefinitionKey, ModuleId>;
pub type LocalAtomId = rue_air::LocalAtomId<StableDefinitionKey, ModuleId>;
pub type StableSymbolId = rue_air::StableSymbolId<StableDefinitionKey, ModuleId>;

/// The sole machine-symbol encoder for stable semantic identities.
///
/// Fields are encoded with explicit tags and byte lengths. Text bytes are hex
/// encoded so the result is a portable object symbol. The format is lossless:
/// no digest or delimiter ambiguity can turn unequal identities into equal
/// symbols.
#[derive(Debug, Default, Clone, Copy)]
pub struct StableSymbolEncoder;

impl StableSymbolEncoder {
    pub const VERSION: u32 = 1;

    pub fn encode(symbol: &StableSymbolId) -> String {
        match symbol {
            StableSymbolId::Callable(StableCallableId::Runtime(helper)) => {
                helper.symbol().to_owned()
            }
            StableSymbolId::ReservedRuntime(export) => export.symbol().to_owned(),
            _ => {
                // Mangled symbols run to a few hundred bytes. `format!`
                // returns an exact-fit allocation, so the prefix alone was
                // followed by a growth for the first encoded field and several
                // more after it.
                let mut output = String::with_capacity(256);
                write!(output, "__rue_sem_v{}_", Self::VERSION)
                    .expect("writing to a String cannot fail");
                encode_symbol(symbol, &mut output);
                output
            }
        }
    }
}

/// Relocate a compiler anonymous key to the request-independent stable content
/// both the digest and the RUE-1114 collision total order are computed over.
///
/// This is the ONE relocation: a caller that ordered compiler keys directly
/// would be ordering `StableDefinitionKey`s, which is not the same total order
/// the semantic engines can reproduce from durable state alone.
fn relocate_anonymous_identity(
    identity: &AnonymousNominalKey,
) -> rue_air::AnonymousNominalKey<String, String> {
    identity
        .with_canonical_producer()
        .as_ref()
        .try_map_identities::<String, String, std::convert::Infallible>(
            &|definition| {
                Ok(rue_air::stable_digest::stable_definition_component(
                    definition.module().logical_path(),
                    definition.name(),
                    definition.owner().map(|owner| owner.name()),
                    definition.kind() as u8,
                ))
            },
            &|module| {
                Ok(rue_air::stable_digest::stable_module_component(
                    module.logical_path(),
                ))
            },
        )
        .expect("compiler anonymous identity relocation to stable content is infallible")
}

pub(crate) fn anonymous_nominal_digest(identity: &AnonymousNominalKey) -> u128 {
    rue_air::stable_digest::stable_anonymous_identity_digest(&relocate_anonymous_identity(identity))
}

/// The deterministic spelling decision for one COMPLETE reached set of
/// anonymous nominals (RUE-1114).
///
/// A digest owned by a single producer-nominal identity is absent from the plan
/// and keeps its bare spelling, so a collision-free program — every program
/// today — produces byte-identical symbols to the pre-plan compiler. A digest
/// verified to be shared by distinct identities ranks each member under the
/// stable-content total order, and every member is spelled with its explicit
/// ordinal.
///
/// The plan is a pure function of the reached SET: it is built by folding into
/// ordered maps before any ordinal exists, so discovery order, scheduling, and
/// cold/warm reuse cannot move a member. Two consumers that hold the same
/// reached set therefore agree without threading the plan between them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AnonymousSymbolPlan {
    ordinals: std::collections::BTreeMap<AnonymousNominalKey, u32>,
}

impl AnonymousSymbolPlan {
    /// The plan of a reached set with no verified digest collision. Every
    /// spelling through it is the bare digest.
    pub(crate) const EMPTY: Self = Self {
        ordinals: std::collections::BTreeMap::new(),
    };

    pub(crate) fn for_reached_set<'a>(
        identities: impl IntoIterator<Item = &'a AnonymousNominalKey>,
    ) -> Self {
        Self::from_digested(identities.into_iter().map(|identity| {
            let stable = relocate_anonymous_identity(identity);
            let digest = rue_air::stable_digest::stable_anonymous_identity_digest(&stable);
            (
                digest,
                identity.with_canonical_producer().into_owned(),
                stable,
            )
        }))
    }

    /// The same rule over narrowed digests, so a forced-collision test exercises
    /// the production ranking rather than a test-only copy of it. The mirror of
    /// the body-closure `force_body_closure_anonymous_digest_for_test` hook:
    /// real digests never collide, so the rule is otherwise unreachable.
    #[cfg(test)]
    pub(crate) fn with_forced_digests<'a>(
        entries: impl IntoIterator<Item = (u128, &'a AnonymousNominalKey)>,
    ) -> Self {
        Self::from_digested(entries.into_iter().map(|(digest, identity)| {
            (
                digest,
                identity.with_canonical_producer().into_owned(),
                relocate_anonymous_identity(identity),
            )
        }))
    }

    fn from_digested(
        entries: impl IntoIterator<
            Item = (
                u128,
                AnonymousNominalKey,
                rue_air::AnonymousNominalKey<String, String>,
            ),
        >,
    ) -> Self {
        let entries = entries.into_iter().collect::<Vec<_>>();
        let by_stable_content = rue_air::stable_digest::anonymous_symbol_ordinals(
            entries
                .iter()
                .map(|(digest, _, stable)| (*digest, stable.clone())),
        );
        Self {
            ordinals: entries
                .into_iter()
                .filter_map(|(_, key, stable)| {
                    by_stable_content
                        .get(&stable)
                        .map(|&ordinal| (key, ordinal))
                })
                .collect(),
        }
    }

    /// The disambiguating ordinal of one identity, or `None` when its digest
    /// has a single owner. Consulted in canonical-producer form, the form the
    /// plan is keyed by.
    pub(crate) fn ordinal(&self, identity: &AnonymousNominalKey) -> Option<u32> {
        self.ordinals
            .get(identity.with_canonical_producer().as_ref())
            .copied()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ordinals.is_empty()
    }

    /// The number of reached identities whose spelling this plan disambiguates.
    pub(crate) fn disambiguated_len(&self) -> usize {
        self.ordinals.len()
    }
}

/// Spell an anonymous nominal through the same stable-content digest used by
/// both semantic engines. The digest is presentation only; the full key remains
/// the identity used by queries and relocation.
///
/// This is the spelling of an identity whose digest has one owner. A caller
/// holding the complete reached set spells through
/// [`anonymous_nominal_source_symbol_in`] instead, so a verified collision is
/// disambiguated rather than conflated.
pub(crate) fn anonymous_nominal_source_symbol(identity: &AnonymousNominalKey) -> String {
    anonymous_nominal_source_symbol_in(&AnonymousSymbolPlan::EMPTY, identity)
}

pub(crate) fn anonymous_nominal_source_symbol_in(
    plan: &AnonymousSymbolPlan,
    identity: &AnonymousNominalKey,
) -> String {
    anonymous_nominal_source_symbol_with_plan_and_digest(
        plan,
        identity,
        anonymous_nominal_digest(identity),
    )
}

/// Spell an anonymous nominal when the canonical digest was already retained
/// by its durable fact. The identity still supplies the kind and collision-plan
/// lookup; the digest is never recovered from presentation text.
pub(crate) fn anonymous_nominal_source_symbol_from_digest(
    identity: &AnonymousNominalKey,
    digest: u128,
) -> String {
    anonymous_nominal_source_symbol_with_plan_and_digest(
        &AnonymousSymbolPlan::EMPTY,
        identity,
        digest,
    )
}

fn anonymous_nominal_source_symbol_with_plan_and_digest(
    plan: &AnonymousSymbolPlan,
    identity: &AnonymousNominalKey,
    digest: u128,
) -> String {
    let kind = match identity.kind {
        AnonymousNominalKind::Struct => "struct",
        AnonymousNominalKind::Enum => "enum",
    };
    let component =
        rue_air::stable_digest::stable_anonymous_symbol_component(digest, plan.ordinal(identity));
    format!("__anon_{kind}_{component}")
}

pub(crate) fn anonymous_member_source_symbol(identity: &FunctionInstanceKey) -> Option<String> {
    anonymous_member_source_symbol_in(&AnonymousSymbolPlan::EMPTY, identity)
}

pub(crate) fn anonymous_member_source_symbol_in(
    plan: &AnonymousSymbolPlan,
    identity: &FunctionInstanceKey,
) -> Option<String> {
    let FunctionInstanceKey::AnonymousMember { owner, member } = identity else {
        return None;
    };
    let TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(owner)) = owner.as_ref() else {
        return None;
    };
    Some(format!(
        "{}.{}",
        anonymous_nominal_source_symbol_in(plan, owner),
        member.name
    ))
}

/// Spell a named nominal through the same unconditional module qualification
/// used by AIR's type pool. Canonical language-item and reserved builtin types
/// retain their bare ABI names.
pub(crate) fn named_nominal_source_symbol(identity: &StableDefinitionKey) -> Option<String> {
    match identity.kind() {
        crate::StableDefinitionKind::Struct => {
            if identity.module().is_trusted_standard_library()
                && rue_air::LangItem::from_standard_library_nominal(
                    identity.module().logical_path(),
                    identity.name(),
                )
                .is_some()
            {
                return Some(identity.name().to_owned());
            }
        }
        crate::StableDefinitionKind::Enum => {
            if rue_builtins::is_reserved_enum_name(identity.name()) {
                return Some(identity.name().to_owned());
            }
        }
        _ => return None,
    }
    Some(format!(
        "{}${}",
        identity.name(),
        rue_air::mangle_symbol_component(&rue_air::normalize_module_path(
            identity.module().logical_path()
        ))
    ))
}

pub(crate) fn type_instance_from_semantic(
    value: &rue_air::SemanticImportType<StableDefinitionKey, ModuleId>,
) -> Option<TypeInstanceKey> {
    use rue_air::SemanticImportType as T;
    Some(match value {
        T::I8 => TypeInstanceKey::I8,
        T::I16 => TypeInstanceKey::I16,
        T::I32 => TypeInstanceKey::I32,
        T::I64 => TypeInstanceKey::I64,
        T::U8 => TypeInstanceKey::U8,
        T::U16 => TypeInstanceKey::U16,
        T::U32 => TypeInstanceKey::U32,
        T::U64 => TypeInstanceKey::U64,
        T::Bool => TypeInstanceKey::Bool,
        T::Unit => TypeInstanceKey::Unit,
        T::Never => TypeInstanceKey::Never,
        T::ComptimeType => TypeInstanceKey::ComptimeType,
        T::BuiltinNominal { name, kind } => TypeInstanceKey::BuiltinNominal {
            kind: match kind {
                rue_air::SemanticImportNominalKind::Struct => AnonymousNominalKind::Struct,
                rue_air::SemanticImportNominalKind::Enum => AnonymousNominalKind::Enum,
            },
            name: name.clone(),
        },
        T::Nominal(key) => TypeInstanceKey::Nominal(NominalInstanceKey::Named(key.clone())),
        T::AnonymousNominal(key) => {
            TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(key.clone()))
        }
        T::Array { element, len } => TypeInstanceKey::Array {
            element: Box::new(type_instance_from_semantic(element)?),
            len: *len,
        },
        T::Slice { element, name } => TypeInstanceKey::Slice {
            element: Box::new(type_instance_from_semantic(element)?),
            name: name.clone(),
        },
        T::PtrConst(pointee) => {
            TypeInstanceKey::PtrConst(Box::new(type_instance_from_semantic(pointee)?))
        }
        T::PtrMut(pointee) => {
            TypeInstanceKey::PtrMut(Box::new(type_instance_from_semantic(pointee)?))
        }
        T::Module(module) => TypeInstanceKey::Module(module.clone()),
        T::GenericParameter(index) => TypeInstanceKey::GenericParameter(*index),
    })
}

pub(crate) fn argument_value_from_semantic(
    value: &rue_air::SemanticImportConstValue<StableDefinitionKey, ModuleId>,
) -> Option<CanonicalArgumentValue> {
    use rue_air::SemanticImportConstValue as V;
    Some(match value {
        V::Integer(value) => CanonicalArgumentValue::Integer(*value),
        V::Bool(value) => CanonicalArgumentValue::Bool(*value),
        V::Type(value) => {
            CanonicalArgumentValue::Type(Box::new(type_instance_from_semantic(value)?))
        }
        V::Function(value) => CanonicalArgumentValue::Function(Box::new(
            FunctionInstanceKey::Definition(value.clone()),
        )),
        V::Unit => CanonicalArgumentValue::Unit,
        V::String(value) => CanonicalArgumentValue::String(value.clone()),
    })
}

pub(crate) fn function_instance_from_specialization(
    value: &rue_air::SemanticSpecializationIdentity<StableDefinitionKey, ModuleId>,
) -> Option<FunctionInstanceKey> {
    let types = value
        .type_arguments
        .iter()
        .map(type_instance_from_semantic)
        .collect::<Option<Vec<_>>>()?;
    let values = value
        .value_arguments
        .iter()
        .map(argument_value_from_semantic)
        .collect::<Option<Vec<_>>>()?;
    Some(FunctionInstanceKey::Specialization {
        base: Box::new(FunctionInstanceKey::Definition(value.base.clone())),
        arguments: CanonicalArguments {
            types: types.into(),
            values: values.into(),
        },
    })
}

fn tag(output: &mut String, value: u32) {
    output.push('t');
    decimal(output, value);
    output.push('_');
}

fn number<I: itoa::Integer>(output: &mut String, value: I) {
    let mut buffer = itoa::Buffer::new();
    let value = buffer.format(value);
    output.push('n');
    decimal(output, value.len());
    output.push('_');
    output.push_str(value);
}

fn decimal<I: itoa::Integer>(output: &mut String, value: I) {
    let mut buffer = itoa::Buffer::new();
    output.push_str(buffer.format(value));
}

fn bytes(output: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    /// Source bytes rendered per `push_str`. Two hex digits each, so the
    /// staging buffer is twice this.
    const CHUNK: usize = 32;

    output.push('s');
    decimal(output, value.len());
    output.push('_');
    // Every module path and definition name in every mangled symbol comes
    // through here, so the digits are staged and appended a chunk at a time.
    // Pushing them one `char` at a time re-checked capacity and re-encoded
    // UTF-8 for each of the two digits of every byte, and grew the string
    // repeatedly on the way.
    output.reserve(value.len() * 2);
    let mut staged = [0u8; CHUNK * 2];
    for block in value.as_bytes().chunks(CHUNK) {
        for (index, &byte) in block.iter().enumerate() {
            staged[index * 2] = HEX[usize::from(byte >> 4)];
            staged[index * 2 + 1] = HEX[usize::from(byte & 0x0f)];
        }
        output.push_str(
            std::str::from_utf8(&staged[..block.len() * 2]).expect("hex digits are ASCII"),
        );
    }
}

fn sequence<T>(output: &mut String, values: &[T], mut encode: impl FnMut(&T, &mut String)) {
    output.push('q');
    decimal(output, values.len());
    output.push('_');
    let mut field = String::new();
    for value in values {
        field.clear();
        encode(value, &mut field);
        decimal(output, field.len());
        output.push('_');
        output.push_str(&field);
    }
}

fn encode_definition(value: &StableDefinitionKey, output: &mut String) {
    bytes(output, value.module().as_str());
    encode_definition_namespace(value.namespace(), output);
    encode_definition_kind(value.kind(), output);
    bytes(output, value.name());
    match value.owner() {
        Some(owner) => {
            tag(output, 1);
            encode_named_nominal(owner, output);
        }
        None => tag(output, 0),
    }
}

fn encode_named_nominal(value: &StableNamedTypeKey, output: &mut String) {
    bytes(output, value.module().as_str());
    encode_definition_kind(value.kind(), output);
    bytes(output, value.name());
}

fn encode_definition_namespace(value: crate::StableDefinitionNamespace, output: &mut String) {
    tag(
        output,
        match value {
            crate::StableDefinitionNamespace::Value => 0,
            crate::StableDefinitionNamespace::Type => 1,
            crate::StableDefinitionNamespace::Destructor => 2,
            crate::StableDefinitionNamespace::Method => 3,
        },
    );
}

fn encode_definition_kind(value: crate::StableDefinitionKind, output: &mut String) {
    tag(
        output,
        match value {
            crate::StableDefinitionKind::Function => 0,
            crate::StableDefinitionKind::Struct => 1,
            crate::StableDefinitionKind::Enum => 2,
            crate::StableDefinitionKind::ValueConst => 3,
            crate::StableDefinitionKind::ModuleBinding => 4,
            crate::StableDefinitionKind::Destructor => 5,
            crate::StableDefinitionKind::Method => 6,
            crate::StableDefinitionKind::AssociatedFunction => 7,
        },
    );
}

fn encode_nominal_kind(value: AnonymousNominalKind, output: &mut String) {
    tag(
        output,
        match value {
            AnonymousNominalKind::Struct => 0,
            AnonymousNominalKind::Enum => 1,
        },
    );
}

fn encode_anchor(value: &StructuralAnchor, output: &mut String) {
    sequence(output, value.segments(), |segment, output| match segment {
        StructuralPathSegment::Body => tag(output, 0),
        StructuralPathSegment::ParameterType(index) => {
            tag(output, 1);
            number(output, *index);
        }
        StructuralPathSegment::ReturnType => tag(output, 2),
        StructuralPathSegment::Statement(index) => {
            tag(output, 3);
            number(output, *index);
        }
        StructuralPathSegment::Operand(index) => {
            tag(output, 4);
            number(output, *index);
        }
        StructuralPathSegment::Branch(index) => {
            tag(output, 5);
            number(output, *index);
        }
        StructuralPathSegment::MatchArm(index) => {
            tag(output, 6);
            number(output, *index);
        }
        StructuralPathSegment::FieldType(index) => {
            tag(output, 7);
            number(output, *index);
        }
        StructuralPathSegment::VariantPayload { variant, payload } => {
            tag(output, 8);
            number(output, *variant);
            number(output, *payload);
        }
        StructuralPathSegment::Method(index) => {
            tag(output, 9);
            number(output, *index);
        }
        StructuralPathSegment::AnonymousType(index) => {
            tag(output, 10);
            number(output, *index);
        }
        StructuralPathSegment::StringLiteral(index) => {
            tag(output, 11);
            number(output, *index);
        }
        StructuralPathSegment::ReadOnlyData(index) => {
            tag(output, 12);
            number(output, *index);
        }
    });
}

fn encode_arguments(value: &CanonicalArguments, output: &mut String) {
    sequence(output, &value.types, encode_type);
    sequence(output, &value.values, encode_argument_value);
}

fn encode_argument_value(value: &CanonicalArgumentValue, output: &mut String) {
    match value {
        CanonicalArgumentValue::Integer(value) => {
            tag(output, 0);
            number(output, *value);
        }
        CanonicalArgumentValue::Bool(value) => {
            tag(output, 1);
            number(output, u8::from(*value));
        }
        CanonicalArgumentValue::Type(value) => {
            tag(output, 2);
            encode_type(value, output);
        }
        CanonicalArgumentValue::Function(value) => {
            tag(output, 3);
            encode_function(value, output);
        }
        CanonicalArgumentValue::Unit => tag(output, 4),
        CanonicalArgumentValue::String(value) => {
            tag(output, 5);
            bytes(output, value);
        }
    }
}

fn encode_producer(value: &StableProducerId, output: &mut String) {
    match value {
        StableProducerId::Definition(value) => {
            tag(output, 0);
            encode_definition(value, output);
        }
        StableProducerId::Function(value) => {
            tag(output, 1);
            encode_function(value, output);
        }
    }
}

fn encode_type(value: &TypeInstanceKey, output: &mut String) {
    match value {
        TypeInstanceKey::I8 => tag(output, 0),
        TypeInstanceKey::I16 => tag(output, 1),
        TypeInstanceKey::I32 => tag(output, 2),
        TypeInstanceKey::I64 => tag(output, 3),
        TypeInstanceKey::U8 => tag(output, 4),
        TypeInstanceKey::U16 => tag(output, 5),
        TypeInstanceKey::U32 => tag(output, 6),
        TypeInstanceKey::U64 => tag(output, 7),
        TypeInstanceKey::Bool => tag(output, 8),
        TypeInstanceKey::Unit => tag(output, 9),
        TypeInstanceKey::Never => tag(output, 10),
        TypeInstanceKey::ComptimeType => tag(output, 11),
        TypeInstanceKey::BuiltinNominal { kind, name } => {
            tag(output, 12);
            encode_nominal_kind(*kind, output);
            bytes(output, name);
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Builtin { kind, name }) => {
            tag(output, 21);
            encode_nominal_kind(*kind, output);
            bytes(output, name);
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Named(value)) => {
            tag(output, 13);
            encode_definition(value, output);
        }
        TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(value)) => {
            tag(output, 14);
            encode_nominal_kind(value.kind, output);
            encode_producer(&value.producer, output);
            encode_anchor(&value.anchor, output);
            encode_arguments(&value.arguments, output);
        }
        TypeInstanceKey::Array { element, len } => {
            tag(output, 15);
            encode_type(element, output);
            number(output, *len);
        }
        TypeInstanceKey::PtrConst(value) => {
            tag(output, 16);
            encode_type(value, output);
        }
        TypeInstanceKey::PtrMut(value) => {
            tag(output, 17);
            encode_type(value, output);
        }
        TypeInstanceKey::Module(value) => {
            tag(output, 18);
            bytes(output, value.as_str());
        }
        TypeInstanceKey::GenericParameter(value) => {
            tag(output, 19);
            number(output, *value);
        }
        TypeInstanceKey::Slice { element, name } => {
            tag(output, 20);
            encode_type(element, output);
            bytes(output, name);
        }
    }
}

fn encode_function(value: &FunctionInstanceKey, output: &mut String) {
    match value {
        FunctionInstanceKey::Definition(value) => {
            tag(output, 0);
            encode_definition(value, output);
        }
        FunctionInstanceKey::Specialization { base, arguments } => {
            tag(output, 1);
            encode_function(base, output);
            encode_arguments(arguments, output);
        }
        FunctionInstanceKey::AnonymousMember { owner, member } => {
            tag(output, 2);
            encode_type(owner, output);
            tag(
                output,
                match member.kind {
                    AnonymousMemberKind::Method => 0,
                    AnonymousMemberKind::AssociatedFunction => 1,
                    AnonymousMemberKind::Destructor => 2,
                },
            );
            bytes(output, &member.name);
        }
        FunctionInstanceKey::DropGlue(value) => {
            tag(output, 3);
            encode_type(value, output);
        }
    }
}

fn encode_symbol(value: &StableSymbolId, output: &mut String) {
    match value {
        StableSymbolId::Callable(StableCallableId::Function(value)) => {
            tag(output, 0);
            encode_function(value, output);
        }
        StableSymbolId::Callable(StableCallableId::Runtime(value)) => {
            tag(output, 1);
            bytes(output, value.symbol());
        }
        StableSymbolId::Callable(StableCallableId::Compiler(value)) => {
            tag(output, 2);
            tag(
                output,
                match value {
                    CompilerCallableId::ProgramEntry => 0,
                },
            );
        }
        StableSymbolId::ReservedRuntime(value) => {
            tag(output, 3);
            bytes(output, value.symbol());
        }
        StableSymbolId::LocalAtom(value) => {
            tag(output, 4);
            encode_function(&value.producer, output);
            tag(
                output,
                match value.kind {
                    LocalAtomKind::String => 0,
                    LocalAtomKind::ReadOnlyData => 1,
                    LocalAtomKind::WritableData => 2,
                },
            );
            encode_anchor(&value.anchor, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use super::*;
    use crate::{StableDefinitionKind, StableDefinitionNamespace};
    use rue_runtime_abi::{ReservedExportId, RuntimeHelperId};

    fn definition(module: &str, name: &str) -> StableDefinitionKey {
        StableDefinitionKey::for_test(
            ModuleId::from_logical_path(module).unwrap(),
            StableDefinitionNamespace::Value,
            StableDefinitionKind::Function,
            Arc::<str>::from(name),
            None,
        )
    }

    fn symbol(definition: StableDefinitionKey) -> StableSymbolId {
        StableSymbolId::Callable(StableCallableId::Function(FunctionInstanceKey::Definition(
            definition,
        )))
    }

    #[test]
    fn framing_helpers_preserve_the_version_one_wire_format() {
        let mut encoded = String::new();
        tag(&mut encoded, u32::MAX);
        assert_eq!(encoded, "t4294967295_");

        encoded.clear();
        number(&mut encoded, i128::MIN);
        assert_eq!(encoded, "n40_-170141183460469231731687303715884105728");

        encoded.clear();
        bytes(&mut encoded, "a_\0é");
        assert_eq!(encoded, "s5_615f00c3a9");

        encoded.clear();
        sequence(&mut encoded, &[i128::MIN, 0, 10], |value, output| {
            number(output, *value);
        });
        assert_eq!(
            encoded,
            "q3_44_n40_-1701411834604692317316873037158841057284_n1_05_n2_10"
        );
    }

    #[test]
    fn framed_encoding_is_injective_for_adversarial_names() {
        let symbols = [
            symbol(definition("a", "b$c")),
            symbol(definition("a$b", "c")),
            symbol(definition("a", "b")),
            symbol(definition("a", "b_0_")),
            symbol(definition("a/x", "b")),
        ];
        let encoded = symbols
            .iter()
            .map(StableSymbolEncoder::encode)
            .collect::<BTreeSet<_>>();
        assert_eq!(encoded.len(), symbols.len());
    }

    #[test]
    fn anonymous_identity_includes_producer_anchor_arguments_and_kind() {
        let producer = definition("m", "make");
        let make = |kind, ordinal, argument| {
            TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(AnonymousNominalKey {
                kind,
                producer: StableProducerId::Definition(producer.clone()),
                anchor: StructuralAnchor::new(vec![StructuralPathSegment::AnonymousType(ordinal)]),
                arguments: CanonicalArguments {
                    types: vec![argument].into(),
                    values: Arc::new([]),
                },
            }))
        };
        let baseline = make(AnonymousNominalKind::Struct, 0, TypeInstanceKey::I32);
        assert_eq!(
            baseline,
            make(AnonymousNominalKind::Struct, 0, TypeInstanceKey::I32)
        );
        assert_ne!(
            baseline,
            make(AnonymousNominalKind::Enum, 0, TypeInstanceKey::I32)
        );
        assert_ne!(
            baseline,
            make(AnonymousNominalKind::Struct, 1, TypeInstanceKey::I32)
        );
        assert_ne!(
            baseline,
            make(AnonymousNominalKind::Struct, 0, TypeInstanceKey::Bool)
        );
    }

    /// Three distinct anonymous sites: two kinds under one producer plus a
    /// second producer, so struct/enum parity and producer separation are both
    /// covered by one forced collision class.
    fn anonymous_sites() -> Vec<AnonymousNominalKey> {
        let site = |producer: &str, kind, ordinal| AnonymousNominalKey {
            kind,
            producer: StableProducerId::Definition(definition("m", producer)),
            anchor: StructuralAnchor::new(vec![
                StructuralPathSegment::Body,
                StructuralPathSegment::AnonymousType(ordinal),
            ]),
            arguments: CanonicalArguments::default(),
        };
        vec![
            site("First", AnonymousNominalKind::Struct, 0),
            site("First", AnonymousNominalKind::Enum, 1),
            site("Second", AnonymousNominalKind::Struct, 0),
        ]
    }

    fn forced_symbols(digest: u128, sites: &[AnonymousNominalKey]) -> Vec<String> {
        let plan = AnonymousSymbolPlan::with_forced_digests(
            sites.iter().map(|identity| (digest, identity)),
        );
        sites
            .iter()
            .map(|identity| anonymous_nominal_source_symbol_in(&plan, identity))
            .collect()
    }

    /// Real digests do not collide, so the plan of a reached set is empty and
    /// every spelling is byte-identical to the bare-digest spelling that
    /// predates the disambiguation rule. This is the no-regression guard for
    /// every existing symbol table.
    #[test]
    fn a_collision_free_reached_set_leaves_every_spelling_unchanged() {
        let sites = anonymous_sites();
        let plan = AnonymousSymbolPlan::for_reached_set(sites.iter());
        assert!(plan.is_empty());
        assert_eq!(plan.disambiguated_len(), 0);
        for identity in &sites {
            assert_eq!(plan.ordinal(identity), None);
            let symbol = anonymous_nominal_source_symbol_in(&plan, identity);
            assert_eq!(symbol, anonymous_nominal_source_symbol(identity));
            let kind = match identity.kind {
                AnonymousNominalKind::Struct => "struct",
                AnonymousNominalKind::Enum => "enum",
            };
            assert_eq!(
                symbol,
                format!("__anon_{kind}_{:032x}", anonymous_nominal_digest(identity))
            );
        }
    }

    /// A verified collision spells every member distinctly, and no member keeps
    /// the unqualified spelling: the rule has no first-registrant winner. The
    /// owner's member symbols follow the owner's ordinal, so a destructor never
    /// relocates onto the other producer's glue.
    #[test]
    fn a_forced_collision_disambiguates_every_member_including_its_methods() {
        let digest = 0x1114;
        let sites = anonymous_sites();
        let plan =
            AnonymousSymbolPlan::with_forced_digests(sites.iter().map(|site| (digest, site)));
        assert_eq!(plan.disambiguated_len(), sites.len());

        let symbols = forced_symbols(digest, &sites);
        assert_eq!(symbols.iter().collect::<BTreeSet<_>>().len(), sites.len());
        for (identity, symbol) in sites.iter().zip(symbols.iter()) {
            let bare = anonymous_nominal_source_symbol(identity);
            assert_ne!(*symbol, bare, "a collision member kept the bare spelling");
            assert!(symbol.starts_with(&bare));
            assert!(symbol.len() <= bare.len() + "$c".len() + 10);
        }

        let owner = &sites[0];
        let member = FunctionInstanceKey::AnonymousMember {
            owner: Box::new(TypeInstanceKey::Nominal(NominalInstanceKey::Anonymous(
                owner.clone(),
            ))),
            member: AnonymousMemberKey {
                kind: AnonymousMemberKind::Destructor,
                name: Arc::from("__drop"),
            },
        };
        assert_eq!(
            anonymous_member_source_symbol_in(&plan, &member).as_deref(),
            Some(format!("{}.__drop", symbols[0]).as_str())
        );
        assert_ne!(
            anonymous_member_source_symbol_in(&plan, &member),
            anonymous_member_source_symbol(&member),
        );
    }

    /// Order independence at the compiler's naming boundary: the symbol table
    /// of a forced collision is identical under every discovery order of the
    /// reached set, which is what makes a rebuild reproducible when producers
    /// are analyzed in different body transactions or scheduling orders.
    #[test]
    fn forced_collision_spelling_is_independent_of_reached_set_order() {
        let digest = 0x1114;
        let sites = anonymous_sites();
        let expected = sites
            .iter()
            .cloned()
            .zip(forced_symbols(digest, &sites))
            .collect::<BTreeMap<_, _>>();
        let mut orders = 0;
        for a in 0..3 {
            for b in 0..2 {
                let mut remaining = sites.clone();
                let first = remaining.remove(a);
                let second = remaining.remove(b);
                let third = remaining.remove(0);
                let permuted = vec![first, second, third];
                let observed = permuted
                    .iter()
                    .cloned()
                    .zip(forced_symbols(digest, &permuted))
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(observed, expected, "discovery order changed a symbol");
                orders += 1;
            }
        }
        assert_eq!(orders, 6);
    }

    /// Stability across recompiles: the plan is derived from the reached set,
    /// so re-deriving it — the warm and successor-revision case, where no
    /// producer changed — reproduces the same symbols, and an unrelated
    /// anonymous site joining the set under its own digest renames nobody.
    #[test]
    fn forced_collision_spelling_is_stable_across_recomputation() {
        let digest = 0x1114;
        let sites = anonymous_sites();
        let first = forced_symbols(digest, &sites);
        assert_eq!(forced_symbols(digest, &sites), first);

        let unrelated = AnonymousNominalKey {
            kind: AnonymousNominalKind::Struct,
            producer: StableProducerId::Definition(definition("m", "Unrelated")),
            anchor: StructuralAnchor::new(vec![StructuralPathSegment::AnonymousType(9)]),
            arguments: CanonicalArguments::default(),
        };
        let widened = AnonymousSymbolPlan::with_forced_digests(
            sites
                .iter()
                .map(|site| (digest, site))
                .chain(std::iter::once((digest ^ 0xff, &unrelated))),
        );
        assert_eq!(widened.ordinal(&unrelated), None);
        for (identity, symbol) in sites.iter().zip(first.iter()) {
            assert_eq!(
                anonymous_nominal_source_symbol_in(&widened, identity),
                *symbol
            );
        }
    }

    #[test]
    fn named_nominal_source_symbols_match_type_pool_qualification() {
        let named = |module, kind, name| {
            StableDefinitionKey::for_test(
                module,
                StableDefinitionNamespace::Type,
                kind,
                Arc::<str>::from(name),
                None,
            )
        };
        let record = named(
            ModuleId::from_validated_canonical("pkg/main.rue"),
            StableDefinitionKind::Struct,
            "Record",
        );
        assert_eq!(
            named_nominal_source_symbol(&record).as_deref(),
            Some("Record$pkg_2fmain_2erue")
        );

        let strbuf = named(
            ModuleId::from_trusted_validated_canonical("\0rue-std/strbuf.rue"),
            StableDefinitionKind::Struct,
            "StrBuf",
        );
        assert_eq!(
            named_nominal_source_symbol(&strbuf).as_deref(),
            Some("StrBuf")
        );
    }

    #[test]
    fn runtime_and_reserved_symbols_preserve_the_abi_manifest() {
        let mut all = BTreeSet::new();
        for helper in RuntimeHelperId::ALL {
            let encoded = StableSymbolEncoder::encode(&StableSymbolId::Callable(
                StableCallableId::Runtime(helper),
            ));
            assert_eq!(encoded, helper.symbol());
            assert!(all.insert(encoded));
        }
        for export in ReservedExportId::ALL {
            let encoded = StableSymbolEncoder::encode(&StableSymbolId::ReservedRuntime(export));
            assert_eq!(encoded, export.symbol());
            assert!(all.insert(encoded));
        }

        let source_entry = StableSymbolEncoder::encode(&StableSymbolId::Callable(
            StableCallableId::Compiler(CompilerCallableId::ProgramEntry),
        ));
        assert_ne!(source_entry, "main");
        assert!(source_entry.starts_with("__rue_sem_v1_"));
        assert!(all.insert(source_entry));
    }

    #[test]
    fn atom_identity_is_occurrence_not_content() {
        let left = LocalAtomId {
            producer: FunctionInstanceKey::Definition(definition("m", "left")),
            kind: LocalAtomKind::String,
            anchor: StructuralAnchor::new(vec![StructuralPathSegment::StringLiteral(0)]),
        };
        let right = LocalAtomId {
            producer: FunctionInstanceKey::Definition(definition("m", "right")),
            kind: LocalAtomKind::String,
            anchor: StructuralAnchor::new(vec![StructuralPathSegment::StringLiteral(0)]),
        };
        assert_ne!(left, right);
    }

    #[test]
    fn atom_encoding_frames_producer_kind_and_full_structural_anchor() {
        let producers = [definition("m", "left"), definition("m", "right")];
        let kinds = [
            LocalAtomKind::String,
            LocalAtomKind::ReadOnlyData,
            LocalAtomKind::WritableData,
        ];
        let anchors = [
            StructuralAnchor::new(vec![
                StructuralPathSegment::Body,
                StructuralPathSegment::Statement(1),
                StructuralPathSegment::StringLiteral(0),
            ]),
            StructuralAnchor::new(vec![
                StructuralPathSegment::Body,
                StructuralPathSegment::Statement(10),
                StructuralPathSegment::StringLiteral(0),
            ]),
        ];
        let encoded = producers
            .iter()
            .flat_map(|producer| {
                kinds.iter().flat_map(|kind| {
                    anchors.iter().map(|anchor| {
                        StableSymbolEncoder::encode(&StableSymbolId::LocalAtom(LocalAtomId {
                            producer: FunctionInstanceKey::Definition(producer.clone()),
                            kind: *kind,
                            anchor: anchor.clone(),
                        }))
                    })
                })
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(encoded.len(), producers.len() * kinds.len() * anchors.len());
        assert!(
            encoded
                .iter()
                .all(|symbol| symbol.starts_with("__rue_sem_v1_"))
        );
    }
}
