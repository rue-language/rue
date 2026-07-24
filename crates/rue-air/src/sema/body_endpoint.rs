//! Endpoint / definition-selection resolution for exact-one-body analysis.
//!
//! Family 1A of the RUE-1091 analyzer rewire (slice r1a). The endpoint,
//! nominal, and module reads that `one_body.rs` performs while turning a
//! canonical body request or reference into a concrete declaration, method,
//! destructor, or materialized [`Type`] no longer touch the semantic epoch
//! tables directly. They flow through [`BodyEndpointProvider`] — the
//! value/definition-world analog of [`crate::SemanticTypeSyntaxProvider`] — so
//! the selection *logic* is provider-generic and a later slice can supply the
//! same facts from a body-fact provider + overlay instead of the epoch `Sema`.
//!
//! [`EpochFacts`] is the one production implementation: each operation is the
//! verbatim epoch-table read the inline `one_body.rs` code performed, so the
//! hoist is byte-identical. Every operation is `&self` and returns owned or
//! `Copy` data, so a caller inside an `&mut Sema` stack constructs a
//! short-lived [`EpochFacts`] per resolution (mirroring `SemaTypeSyntaxProvider`)
//! without retaining a borrow across the surrounding mutations.

use lasso::Spur;
use rue_rir::InstRef;
use rue_span::FileId;

use super::BodySema;
use super::anon_structs::IssuedAnonymousNominalKey;
use super::declaration_index::RirDestructorDeclaration;
use super::info::{FunctionInfo, MethodInfo};
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{
    SemanticDefinitionEndpoint, SemanticDefinitionToken, SemanticModuleEndpoint,
    SemanticModuleToken, StableDefinitionKind,
};

/// The exact endpoint/definition-selection fact boundary consumed by
/// `one_body.rs`. Every operation answers one point query against the
/// declaration universe and returns owned/`Copy` data — no borrowed epoch table
/// or live `Sema` handle escapes.
pub(in crate::sema) trait BodyEndpointProvider {
    /// Intern a source name to its symbol, or `None` if never interned. Mirrors
    /// `interner.get`.
    fn name_symbol(&self, name: &str) -> Option<Spur>;

    /// The current stable endpoint for a definition token.
    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint>;

    /// The current stable endpoint for a module token.
    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint>;

    /// The internal free-function symbol declared as `(file, name)`.
    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur>;

    /// The struct id declared as `(file, name)`.
    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId>;

    /// The enum id declared as `(file, name)`.
    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId>;

    /// The built-in or compiler-generated struct id for a bare name, preferring
    /// the built-in table. Mirrors `builtin_structs.or_else(generated_structs)`.
    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId>;

    /// The compiler-generated struct id for a bare name.
    fn generated_struct(&self, name: Spur) -> Option<StructId>;

    /// The built-in enum id for a bare name.
    fn builtin_enum(&self, name: Spur) -> Option<EnumId>;

    /// The struct id for an issued anonymous nominal identity.
    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId>;

    /// The enum id for an issued anonymous nominal identity.
    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId>;

    /// Whether a bare name is a built-in or compiler-generated struct.
    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool;

    /// Whether a bare name is a built-in enum.
    fn is_builtin_enum(&self, name: Spur) -> bool;

    /// The signature/binding info for an internal free-function symbol.
    fn function_info(&self, name: Spur) -> Option<FunctionInfo>;

    /// The signature/binding info for a `(struct, name)` member.
    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo>;

    /// The source name a specialized/internal function name derives from.
    /// Mirrors `Sema::source_function_name` (identity when unmapped).
    fn source_function_name(&self, name: Spur) -> Spur;

    /// The first free-function RIR declaration for `(source, file)`.
    fn first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef>;

    /// The named-method RIR declaration for `(struct, name)`.
    fn named_method_declaration(&self, struct_id: StructId, name: Spur) -> Option<InstRef>;

    /// The destructor declaration record for `(file, type_name)`.
    fn destructor(&self, file: u32, type_name: Spur) -> Option<RirDestructorDeclaration>;

    /// The module id whose definition lives in `file`.
    fn module_id_for_file(&self, file: u32) -> Option<ModuleId>;

    /// Intern an array type, or `None` on a type-validation failure.
    fn intern_array(&self, element: Type, len: u64) -> Option<Type>;

    /// Intern a `ptr const` type, or `None` on a type-validation failure.
    fn intern_ptr_const(&self, pointee: Type) -> Option<Type>;

    /// Intern a `ptr mut` type, or `None` on a type-validation failure.
    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type>;
}

/// The production [`BodyEndpointProvider`]: every operation is the verbatim
/// epoch-table read the inline `one_body.rs` code performed.
pub(in crate::sema) struct EpochFacts<'s, 'a> {
    sema: &'s BodySema<'a>,
}

/// Construct a short-lived [`EpochFacts`] borrowing `sema` for the duration of
/// one endpoint resolution.
pub(in crate::sema) fn endpoint_facts<'s, 'a>(sema: &'s BodySema<'a>) -> EpochFacts<'s, 'a> {
    EpochFacts { sema }
}

impl BodyEndpointProvider for EpochFacts<'_, '_> {
    fn name_symbol(&self, name: &str) -> Option<Spur> {
        self.sema.interner.get(name)
    }

    fn definition_endpoint(
        &self,
        token: SemanticDefinitionToken,
    ) -> Option<SemanticDefinitionEndpoint> {
        self.sema.stable_definition_endpoints.get(&token).cloned()
    }

    fn module_endpoint(&self, token: SemanticModuleToken) -> Option<SemanticModuleEndpoint> {
        self.sema.stable_module_endpoints.get(&token).copied()
    }

    fn function_by_file_name(&self, file: FileId, name: Spur) -> Option<Spur> {
        self.sema.functions_by_file_name.get(&(file, name)).copied()
    }

    fn struct_by_file_name(&self, file: FileId, name: Spur) -> Option<StructId> {
        self.sema.structs_by_file_name.get(&(file, name)).copied()
    }

    fn enum_by_file_name(&self, file: FileId, name: Spur) -> Option<EnumId> {
        self.sema.enums_by_file_name.get(&(file, name)).copied()
    }

    fn builtin_or_generated_struct(&self, name: Spur) -> Option<StructId> {
        self.sema
            .builtin_structs
            .get(&name)
            .or_else(|| self.sema.generated_structs.get(&name))
            .copied()
    }

    fn generated_struct(&self, name: Spur) -> Option<StructId> {
        self.sema.generated_structs.get(&name).copied()
    }

    fn builtin_enum(&self, name: Spur) -> Option<EnumId> {
        self.sema.builtin_enums.get(&name).copied()
    }

    fn anon_struct(&self, identity: &IssuedAnonymousNominalKey) -> Option<StructId> {
        self.sema.anon_struct_identities.get(identity).copied()
    }

    fn anon_enum(&self, identity: &IssuedAnonymousNominalKey) -> Option<EnumId> {
        self.sema.anon_enum_identities.get(identity).copied()
    }

    fn is_builtin_or_generated_struct(&self, name: Spur) -> bool {
        self.sema.builtin_structs.contains_key(&name)
            || self.sema.generated_structs.contains_key(&name)
    }

    fn is_builtin_enum(&self, name: Spur) -> bool {
        self.sema.builtin_enums.contains_key(&name)
    }

    fn function_info(&self, name: Spur) -> Option<FunctionInfo> {
        self.sema.function_info(name).copied()
    }

    fn method_info(&self, struct_id: StructId, name: Spur) -> Option<MethodInfo> {
        self.sema.method_info((struct_id, name)).copied()
    }

    fn source_function_name(&self, name: Spur) -> Spur {
        self.sema.source_function_name(name)
    }

    fn first_free_function(&self, source: Spur, file_id: FileId) -> Option<InstRef> {
        self.sema
            .declaration_index
            .first_free_function(source, Some(file_id))
    }

    fn named_method_declaration(&self, struct_id: StructId, name: Spur) -> Option<InstRef> {
        self.sema
            .named_method_declarations
            .get(&(struct_id, name))
            .copied()
    }

    fn destructor(&self, file: u32, type_name: Spur) -> Option<RirDestructorDeclaration> {
        self.sema
            .declaration_index
            .destructors()
            .iter()
            .find(|record| record.span.file_id.index() == file && record.type_name == type_name)
            .copied()
    }

    fn module_id_for_file(&self, file: u32) -> Option<ModuleId> {
        (0..self.sema.module_registry.len())
            .map(|index| ModuleId::new(index as u32))
            .find(|id| self.sema.module_registry.get_def(*id).file_id.index() == file)
    }

    fn intern_array(&self, element: Type, len: u64) -> Option<Type> {
        self.sema.type_pool.try_intern_array(element, len).ok()
    }

    fn intern_ptr_const(&self, pointee: Type) -> Option<Type> {
        self.sema.type_pool.try_intern_ptr_const(pointee).ok()
    }

    fn intern_ptr_mut(&self, pointee: Type) -> Option<Type> {
        self.sema.type_pool.try_intern_ptr_mut(pointee).ok()
    }
}

/// Resolve a definition-token function reference to its internal free-function
/// symbol. The endpoint, name interning, and by-file lookup all fail to the
/// same `MissingStableIdentity` in the caller, so a single `None` is faithful.
pub(in crate::sema) fn resolve_free_function_symbol<P: BodyEndpointProvider>(
    facts: &P,
    token: SemanticDefinitionToken,
) -> Option<Spur> {
    let endpoint = facts.definition_endpoint(token)?;
    let symbol = facts.name_symbol(endpoint.name.as_ref())?;
    facts.function_by_file_name(FileId::new(endpoint.file), symbol)
}

/// Materialize a canonical type-instance key into a concrete [`Type`]. The
/// provider-generic form of `one_body::materialize_instance_type`: an exact
/// transcription of the epoch code, with every table read routed through
/// `facts` and every failure mapped to `MissingStableIdentity`.
pub(in crate::sema) fn resolve_instance_type<P: BodyEndpointProvider>(
    facts: &P,
    value: &crate::TypeInstanceKey<SemanticDefinitionToken, SemanticModuleToken>,
) -> Result<Type, crate::SemanticBodyExportFailure> {
    use crate::{AnonymousNominalKind as AK, NominalInstanceKey as N, TypeInstanceKey as T};
    let missing = || crate::SemanticBodyExportFailure::MissingStableIdentity;
    Ok(match value {
        T::I8 => Type::I8,
        T::I16 => Type::I16,
        T::I32 => Type::I32,
        T::I64 => Type::I64,
        T::U8 => Type::U8,
        T::U16 => Type::U16,
        T::U32 => Type::U32,
        T::U64 => Type::U64,
        T::Bool => Type::BOOL,
        T::Unit => Type::UNIT,
        T::Never => Type::NEVER,
        T::ComptimeType => Type::COMPTIME_TYPE,
        T::BuiltinNominal { kind, name } => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            match kind {
                AK::Struct => Type::new_struct(
                    facts
                        .builtin_or_generated_struct(symbol)
                        .ok_or_else(missing)?,
                ),
                AK::Enum => Type::new_enum(facts.builtin_enum(symbol).ok_or_else(missing)?),
            }
        }
        T::Nominal(N::Builtin { kind, name }) => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            match kind {
                AK::Struct => Type::new_struct(
                    facts
                        .builtin_or_generated_struct(symbol)
                        .ok_or_else(missing)?,
                ),
                AK::Enum => Type::new_enum(facts.builtin_enum(symbol).ok_or_else(missing)?),
            }
        }
        T::Nominal(N::Named(token)) => {
            let endpoint = facts.definition_endpoint(*token).ok_or_else(missing)?;
            let symbol = facts
                .name_symbol(endpoint.name.as_ref())
                .ok_or_else(missing)?;
            match endpoint.kind {
                StableDefinitionKind::Struct => Type::new_struct(
                    facts
                        .struct_by_file_name(FileId::new(endpoint.file), symbol)
                        .ok_or_else(missing)?,
                ),
                StableDefinitionKind::Enum => Type::new_enum(
                    facts
                        .enum_by_file_name(FileId::new(endpoint.file), symbol)
                        .ok_or_else(missing)?,
                ),
                _ => return Err(missing()),
            }
        }
        T::Nominal(N::Anonymous(identity)) => match identity.kind {
            AK::Struct => Type::new_struct(facts.anon_struct(identity).ok_or_else(missing)?),
            AK::Enum => Type::new_enum(facts.anon_enum(identity).ok_or_else(missing)?),
        },
        T::Array { element, len } => facts
            .intern_array(resolve_instance_type(facts, element)?, *len)
            .ok_or_else(missing)?,
        T::Slice { name, .. } => {
            let symbol = facts.name_symbol(name.as_ref()).ok_or_else(missing)?;
            Type::new_struct(facts.generated_struct(symbol).ok_or_else(missing)?)
        }
        T::PtrConst(value) => facts
            .intern_ptr_const(resolve_instance_type(facts, value)?)
            .ok_or_else(missing)?,
        T::PtrMut(value) => facts
            .intern_ptr_mut(resolve_instance_type(facts, value)?)
            .ok_or_else(missing)?,
        T::Module(token) => {
            let endpoint = facts.module_endpoint(*token).ok_or_else(missing)?;
            let id = facts
                .module_id_for_file(endpoint.file)
                .ok_or_else(missing)?;
            Type::new_module(id)
        }
        T::GenericParameter(_) => return Err(missing()),
    })
}
