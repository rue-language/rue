//! Provider-generic aggregate, field, and variant resolution.
//!
//! Selection order lives here. Both body-analysis hosts implement the one
//! [`AggregateFacts`] boundary directly.

use ahash::AHashMap;
use std::cell::RefCell;
use std::convert::Infallible;
use std::hash::Hash;

use lasso::Spur;
use rue_rir::{InstData, InstRef, Rir};
use rue_span::FileId;

use super::ConstInfo;
use super::body_identity::{DurableNominalSource, ProviderIdentityContext};
use super::context::ConstValue;
use crate::intern_pool::TypeInternPool;
use crate::semantic_type_resolution::{
    UnqualifiedNominal, UnqualifiedNominalTier, select_unqualified_nominal,
};
use crate::types::{EnumId, ModuleId, StructId, Type};
use crate::{SemanticImportNominalKind, SemanticImportType};

pub(crate) trait AggregateFacts {
    fn aggregate_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn aggregate_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo>;
    fn aggregate_struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId>;
    fn aggregate_enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId>;
    fn aggregate_primitive_type(&self, name: Spur) -> Option<Type>;
    fn aggregate_builtin_struct(&self, name: Spur) -> Option<StructId>;
    fn aggregate_builtin_enum(&self, name: Spur) -> Option<EnumId>;
    fn aggregate_module(&self, module: ModuleId) -> AggregateModuleFact;
    fn aggregate_file_path(&self, file: FileId) -> Option<&str>;
    #[allow(dead_code)]
    fn aggregate_source_path(&self, span: rue_span::Span) -> Option<&str>;
    fn aggregate_visibility_domain(&self, file: FileId) -> crate::SemanticVisibilityDomain {
        crate::SemanticVisibilityDomain::from_file_path(self.aggregate_file_path(file))
    }
}

pub(crate) struct AggregateModuleFact {
    pub(crate) file: FileId,
    #[allow(dead_code)]
    file_path: String,
    import_path: String,
}

impl AggregateModuleFact {
    #[allow(dead_code)]
    pub(crate) fn file_path(&self) -> &str {
        &self.file_path
    }

    pub(crate) fn import_path(&self) -> &str {
        &self.import_path
    }
}

pub(crate) enum StructLiteralHead {
    Bound(Type),
    Named(StructId),
    Absent,
}

pub(crate) enum ModuleTypeMember {
    Struct(StructId),
    Enum(EnumId),
    Const(ConstInfo),
    Absent,
}

fn select_unqualified_aggregate_type<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> Option<UnqualifiedNominal<Type>> {
    select_unqualified_nominal(|tier| {
        Ok::<_, Infallible>(match tier {
            UnqualifiedNominalTier::Substitution => None,
            UnqualifiedNominalTier::LexicalAlias => local_type,
            UnqualifiedNominalTier::FileAlias => {
                facts
                    .aggregate_value_const(file, name)
                    .and_then(|info| match info.value {
                        ConstValue::Type(ty) => Some(ty),
                        _ => None,
                    })
            }
            UnqualifiedNominalTier::Primitive => facts.aggregate_primitive_type(name),
            UnqualifiedNominalTier::Declaration => facts
                .aggregate_struct_in_file(file, name)
                .map(Type::new_struct)
                .or_else(|| facts.aggregate_enum_in_file(file, name).map(Type::new_enum)),
            UnqualifiedNominalTier::Builtin => facts
                .aggregate_builtin_struct(name)
                .map(Type::new_struct)
                .or_else(|| facts.aggregate_builtin_enum(name).map(Type::new_enum)),
        })
    })
    .expect("infallible aggregate nominal selection")
}

/// A nominal type named through a module (`m.Name`), plus the `const` binding
/// it arrived through when the module spells that name as a type alias rather
/// than as a declaration.
pub(crate) struct ModuleNominal<'a, Id> {
    pub(crate) id: Id,
    /// `Some` when `m.Name` selected a type-valued `const` bound in the
    /// module's defining file. The binding's own visibility then governs the
    /// access and the aliased declaration's does not — naming an alias is not
    /// naming the declaration behind it, the rule [`resolve_enum_type_name`]
    /// already reports as "privacy handled" for a const-bound type.
    pub(crate) alias: Option<&'a ConstInfo>,
}

impl ModuleTypeMember {
    /// The enum `m.Name` names: an enum declared in the module's defining file,
    /// or a type-valued `const` alias bound there that resolves to one. `None`
    /// when the member is not an enum at all, which every consumer reads as
    /// "this is not a qualified enum path" rather than as an error.
    pub(crate) fn as_enum(&self) -> Option<ModuleNominal<'_, EnumId>> {
        match self {
            ModuleTypeMember::Enum(id) => Some(ModuleNominal {
                id: *id,
                alias: None,
            }),
            ModuleTypeMember::Const(info) => {
                Self::alias_type(info)?.as_enum().map(|id| ModuleNominal {
                    id,
                    alias: Some(info),
                })
            }
            ModuleTypeMember::Struct(_) | ModuleTypeMember::Absent => None,
        }
    }

    /// Struct analogue of [`Self::as_enum`].
    pub(crate) fn as_struct(&self) -> Option<ModuleNominal<'_, StructId>> {
        match self {
            ModuleTypeMember::Struct(id) => Some(ModuleNominal {
                id: *id,
                alias: None,
            }),
            ModuleTypeMember::Const(info) => {
                Self::alias_type(info)?.as_struct().map(|id| ModuleNominal {
                    id,
                    alias: Some(info),
                })
            }
            ModuleTypeMember::Enum(_) | ModuleTypeMember::Absent => None,
        }
    }

    /// The type a `const` member binds, when it binds one at all. A module
    /// binding (`pub const sub = @import(...)`) also stores a `ConstValue::Type`
    /// — of a module type, which names no nominal — so the nominal accessors
    /// filter it out rather than this helper.
    fn alias_type(info: &ConstInfo) -> Option<Type> {
        match info.value {
            ConstValue::Type(ty) => Some(ty),
            _ => None,
        }
    }
}

pub(crate) fn resolve_enum_type_name<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> Option<(EnumId, bool)> {
    select_unqualified_aggregate_type(facts, local_type, file, name).and_then(|selected| {
        selected
            .value
            .as_enum()
            .map(|id| (id, selected.tier.via_binding()))
    })
}

pub(crate) fn resolve_struct_type_name<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> Option<(StructId, bool)> {
    select_unqualified_aggregate_type(facts, local_type, file, name).and_then(|selected| {
        selected
            .value
            .as_struct()
            .map(|id| (id, selected.tier.via_binding()))
    })
}

pub(crate) fn select_struct_literal_head<P: AggregateFacts>(
    facts: &P,
    local_type: Option<Type>,
    file: FileId,
    name: Spur,
) -> StructLiteralHead {
    let selected = select_unqualified_aggregate_type(facts, local_type, file, name);
    match selected {
        Some(selected) if selected.tier.via_binding() => StructLiteralHead::Bound(selected.value),
        Some(selected) => selected
            .value
            .as_struct()
            .map_or(StructLiteralHead::Absent, StructLiteralHead::Named),
        None => StructLiteralHead::Absent,
    }
}

/// The one source order for a type named through a module (`m.Name`): a struct
/// declared in the module's defining file, then an enum declared there, then a
/// type-valued `const` alias bound there.
///
/// Semantic analysis and inference both drive this order but cannot share one
/// selector: sema needs the alias's whole [`ConstInfo`] to apply E0706, while
/// inference's fact source only exposes types. The order therefore lives here
/// once and each side supplies its own three lookups (RUE-1956).
pub(crate) fn select_module_nominal<T>(
    declared_struct: impl FnOnce() -> Option<T>,
    declared_enum: impl FnOnce() -> Option<T>,
    const_alias: impl FnOnce() -> Option<T>,
) -> Option<T> {
    declared_struct()
        .or_else(declared_enum)
        .or_else(const_alias)
}

/// The canonical selector for a type member named through a module. Every
/// consumer of `m.Name` in type position — qualified enum-variant paths and
/// match-pattern heads, associated-function receivers, module-qualified struct
/// literals, and reading the member as a value — resolves it here, so a `const`
/// type alias is a type member wherever a declaration is (RUE-1956).
pub(crate) fn select_module_type_member<P: AggregateFacts>(
    facts: &P,
    file: FileId,
    name: Spur,
) -> ModuleTypeMember {
    select_module_nominal(
        || {
            facts
                .aggregate_struct_in_file(file, name)
                .map(ModuleTypeMember::Struct)
        },
        || {
            facts
                .aggregate_enum_in_file(file, name)
                .map(ModuleTypeMember::Enum)
        },
        || {
            facts
                .aggregate_module_binding(file, name)
                .or_else(|| facts.aggregate_value_const(file, name))
                .map(ModuleTypeMember::Const)
        },
    )
    .unwrap_or(ModuleTypeMember::Absent)
}

/// The dotted spine of a candidate module path (`lib.inner.deep`), decoded
/// from RIR syntax alone.
pub(crate) struct ModuleSpine {
    /// The name at the root of the `FieldGet` chain.
    pub(crate) root: Spur,
    /// The span of the root `VarRef`, whose file owns the root binding.
    pub(crate) root_span: rue_span::Span,
    /// The field names hanging off the root, in source order.
    pub(crate) fields: Vec<Spur>,
}

/// The one syntactic decoder for a dotted module spine (RUE-1964).
///
/// Pure syntax: it walks the `FieldGet` chain down to its `VarRef` root and
/// reports the root name plus the field names in source order. No fact source,
/// scope, or visibility rule is consulted here, so every consumer — semantic
/// analysis, the comptime engine, and the inference prepass — decodes the same
/// spine and then applies the one shadowing rule and (where privacy is its
/// job) the one per-hop visibility walk to it. Returns `None` for a spine that
/// bottoms out in anything other than a name, which can never be a module path.
pub(crate) fn decode_module_spine(rir: &Rir, inst_ref: InstRef) -> Option<ModuleSpine> {
    let mut fields = Vec::new();
    let mut cursor = inst_ref;
    let (root, root_span) = loop {
        let inst = rir.get(cursor);
        match inst.data {
            InstData::VarRef { name, .. } => break (name, inst.span),
            InstData::FieldGet { base, field } => {
                fields.push(field);
                cursor = base;
            }
            _ => return None,
        }
    };
    fields.reverse();
    Some(ModuleSpine {
        root,
        root_span,
        fields,
    })
}

/// Where a decoded [`ModuleSpine`]'s root binds, after the one shadowing rule
/// (spec 5.1:11) is applied to it.
pub(crate) enum ModuleSpineRoot {
    /// A runtime local or parameter of non-module type carries the name, so it
    /// shadows any import of the same name and this is not a module path at
    /// all — the consumer falls through to ordinary value field/method access.
    Shadowed,
    /// A local or parameter that *is* a module (`let m = lib.geo;`): the
    /// binding is the module, and any remaining segments are its members.
    LocalModule(ModuleId),
    /// Nothing shadows the name: the root is a module binding of the file the
    /// spine is written in, resolved by the canonical module-path walker.
    FileBinding,
}

pub(crate) fn is_accessible<P: AggregateFacts>(
    facts: &P,
    accessing_file: FileId,
    defining_file: FileId,
    is_public: bool,
) -> bool {
    if is_public || accessing_file == defining_file {
        return true;
    }
    let accessing = facts.aggregate_visibility_domain(accessing_file);
    let defining = facts.aggregate_visibility_domain(defining_file);
    defining.is_visible_from(&accessing, is_public)
}

// ---------------------------------------------------------------------------
// `ProviderAggregateFacts` — the aggregate / field / variant ProviderFacts.
//
// This driver answers aggregate facts from the body-scoped identity pool plus
// a request-local overlay for `(file, name) → durable key` and file paths.
//
// The selection ORDER is not this driver's concern: it lives in the
// provider-generic free functions above ([`select_module_type_member`]'s
// struct→enum→const short-circuit and [`select_struct_literal_head`]'s
// bound→const→struct→builtin) which this driver merely supplies facts to, so
// every consumer replays the same candidate order and short-circuits. The
// driver's inherent `select_*` wrappers run those free functions over itself
// and hand back the winner as a pool [`Type`].
//
// This surface is public because rue-compiler supplies the concrete durable
// signature source behind the opaque provider boundary. No aggregate op consults the live
// `BodyFactProvider` boundary — struct/enum-by-file-name, the builtins, and
// `is_accessible` are all answered by the pool + the caller-populated overlay —
// so this driver holds no provider handle and records no provider edge by
// construction.
// ---------------------------------------------------------------------------

/// The winner [`select_module_type_member`] selects, projected to a pool
/// [`Type`] a differential renders index-independently. `Const` reports the
/// installed value-constant or module-binding arm, carrying the nominal the
/// binding aliases when it binds one — the arm through which `m.Alias` is a
/// type member (RUE-1956).
pub enum ProviderModuleMember {
    Struct(Type),
    Enum(Type),
    Const(Option<Type>),
    Absent,
}

/// The winner [`select_struct_literal_head`] selects for an unqualified head, as
/// a pool [`Type`]. `Bound` (a local-type-driven head) never arises through this
/// driver's `local_type = None` wrapper; it is carried for completeness.
pub enum ProviderStructHead {
    Bound(Type),
    Named(Type),
    Absent,
}

/// Query-backed aggregate fact state: answers [`AggregateFacts`] from a body
/// identity pool plus a caller-populated overlay, while the epoch host reads
/// its own tables directly.
///
/// Generic over the pool durable source `S` and the pool's durable nominal key
/// `K` and module `M` (rue-compiler binds `K = StableDefinitionKey`,
/// `M = ModuleId`). The pool lives behind a [`RefCell`] because a nominal is
/// minted on first consult (`&mut` on the pool) while [`AggregateFacts`]'s ops —
/// and therefore the provider-generic `select_*` logic driving them — are
/// `&self`; the borrow is never held across a re-entrant consult, so it never
/// conflicts. The `(file, name)` reverse and file paths are plain maps populated
/// by a caller through `&mut self` registration before any consult.
pub struct ProviderAggregateFacts<K, M, S> {
    identity: ProviderIdentityContext<K, M, S>,
    /// The provider-side analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` key maps: `(file, pool-interned name) → durable key`,
    /// populated on demand as a caller registers a nominal. The `Spur` is the
    /// pool's own interner symbol (the same one the `select_*` wrappers intern
    /// their name argument to), never the shared whole-program interner's.
    by_file_name: AHashMap<(u32, Spur), K>,
    /// Request-local file paths (a body-query input, exactly like the RIR — not a
    /// durable fact the pool mints), owned so [`AggregateFacts::file_path`] hands
    /// back a borrowed `&str` without a seam-signature change. A caller registers
    /// the same physical path the epoch's `get_file_path` returns, so
    /// [`is_accessible`]'s visibility-domain computation matches byte-for-byte.
    file_paths: AHashMap<FileId, String>,
    value_consts: RefCell<AHashMap<(u32, Spur), ConstInfo>>,
    module_bindings: RefCell<AHashMap<(u32, Spur), ConstInfo>>,
}

impl<K, M, S> ProviderAggregateFacts<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    /// Construct the driver over a durable nominal source. The pool is built here
    /// (builtin enums + `str` pre-registered); named nominals are minted lazily on
    /// first consult and the overlay maps start empty.
    pub fn new(source: S) -> Self {
        Self::with_identity(ProviderIdentityContext::new(source))
    }

    /// Construct the driver over the shared identity context used by every
    /// provider fact family for body analysis.
    pub fn with_identity(identity: ProviderIdentityContext<K, M, S>) -> Self {
        Self::with_overlay_identity(identity.fail_closed())
    }

    fn with_overlay_identity(identity: ProviderIdentityContext<K, M, S>) -> Self {
        Self {
            identity,
            by_file_name: AHashMap::new(),
            file_paths: AHashMap::new(),
            value_consts: RefCell::new(AHashMap::new()),
            module_bindings: RefCell::new(AHashMap::new()),
        }
    }

    /// Construct the aggregate fact state from the one task-local provider
    /// authority shared with endpoint and call resolution.
    pub fn with_state(state: &super::ProviderBodyAnalysisState<K, M, S>) -> Self {
        Self::with_overlay_identity(state.identity_context())
    }

    /// Record that a durable nominal key is declared as `(file, name)`, the
    /// provider-side analog of the epoch's `structs_by_file_name` /
    /// `enums_by_file_name` insert. The name is interned into the pool's own
    /// interner so a later `select_*`/`struct_in_file` consult reverses the same
    /// `(file, pool-name)` key.
    /// Callers populate this from durable declaration keys or from an upstream
    /// lookup whose provider edge has already been recorded.
    pub fn register_named_nominal(&mut self, key: K, file: FileId, name: &str) {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return;
        };
        self.by_file_name.insert((file.index(), symbol), key);
    }

    /// Record the request-local physical file path for `file`, so
    /// [`Self::is_accessible`] reproduces the epoch's visibility domain exactly.
    pub fn register_file_path(&mut self, file: FileId, path: &str) {
        self.file_paths.insert(file, path.to_owned());
    }

    pub fn register_value_const(&self, file: FileId, name: &str, info: ConstInfo) {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return;
        };
        self.value_consts
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    /// Recover an installed value constant with its complete assembled payload.
    pub fn value_const(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return None;
        };
        self.value_consts
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    pub fn register_module_binding(&self, file: FileId, name: &str, info: ConstInfo) {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return;
        };
        self.module_bindings
            .borrow_mut()
            .insert((file.index(), name), info);
    }

    /// Recover an installed module binding with its complete assembled payload.
    pub fn module_binding(&self, file: FileId, name: &str) -> Option<ConstInfo> {
        let Ok(name) = self.identity.pool().intern_name(name) else {
            return None;
        };
        self.module_bindings
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    /// Register one durable module and its request-local presentation facts in
    /// the body-local module registry. The returned compact id is the provider
    /// counterpart of the epoch's canonical `ModuleId`; downstream aggregate
    /// spines recover the exact file/import paths through [`AggregateFacts::module`].
    pub fn register_module(
        &self,
        module: M,
        file: FileId,
        file_path: &str,
        import_path: &str,
        durable_id: &str,
    ) -> Option<ModuleId> {
        self.identity
            .modules_mut()
            .register(module, file, file_path, import_path, durable_id)
    }

    /// Owned, index-independent module facts for a registered compact id.
    pub fn module_fact(&self, module: ModuleId) -> Option<(FileId, String, String)> {
        self.identity.modules().get(module).map(|definition| {
            (
                definition.file_id,
                definition.file_path,
                definition.import_path,
            )
        })
    }

    /// (P) The struct declared as `(file, name)`, minted through the pool's 2a
    /// nominal machinery, as a pool [`Type`]. Equal to the epoch's
    /// `structs_by_file_name` lookup under the durable-key bijection.
    pub fn struct_in_file(&self, file: FileId, name: &str) -> Option<Type> {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return None;
        };
        AggregateFacts::aggregate_struct_in_file(self, file, symbol).map(Type::new_struct)
    }

    /// (P) The enum declared as `(file, name)`, as a pool [`Type`].
    pub fn enum_in_file(&self, file: FileId, name: &str) -> Option<Type> {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return None;
        };
        AggregateFacts::aggregate_enum_in_file(self, file, symbol).map(Type::new_enum)
    }

    /// (P) The builtin struct for a bare name (the pool's pre-registered `str`),
    /// as a pool [`Type`]. Names beyond the pre-registered builtin set fail closed
    /// (r6 builtin facts).
    pub fn builtin_struct(&self, name: &str) -> Option<Type> {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return None;
        };
        AggregateFacts::aggregate_builtin_struct(self, symbol).map(Type::new_struct)
    }

    /// (P) The builtin target enum for a bare name, as a pool [`Type`].
    pub fn builtin_enum(&self, name: &str) -> Option<Type> {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return None;
        };
        AggregateFacts::aggregate_builtin_enum(self, symbol).map(Type::new_enum)
    }

    /// Run the provider-generic [`select_module_type_member`] over this driver:
    /// the r1c struct→enum→const short-circuit, driven from the pool. The const
    /// fall-through consults the installed body-local const overlay.
    pub fn select_module_type_member(&self, file: FileId, name: &str) -> ProviderModuleMember {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return ProviderModuleMember::Absent;
        };
        let member = select_module_type_member(self, file, symbol);
        match &member {
            ModuleTypeMember::Struct(id) => ProviderModuleMember::Struct(Type::new_struct(*id)),
            ModuleTypeMember::Enum(id) => ProviderModuleMember::Enum(Type::new_enum(*id)),
            ModuleTypeMember::Const(_) => ProviderModuleMember::Const(
                member
                    .as_enum()
                    .map(|nominal| Type::new_enum(nominal.id))
                    .or_else(|| {
                        member
                            .as_struct()
                            .map(|nominal| Type::new_struct(nominal.id))
                    }),
            ),
            ModuleTypeMember::Absent => ProviderModuleMember::Absent,
        }
    }

    /// Run the provider-generic [`select_struct_literal_head`] over this driver
    /// for an unqualified head (`local_type = None`), including an installed
    /// const arm and the canonical declaration/builtin fallback order.
    pub fn select_struct_literal_head(&self, file: FileId, name: &str) -> ProviderStructHead {
        let Ok(symbol) = self.identity.pool().intern_name(name) else {
            return ProviderStructHead::Absent;
        };
        match select_struct_literal_head(self, None, file, symbol) {
            StructLiteralHead::Bound(ty) => ProviderStructHead::Bound(ty),
            StructLiteralHead::Named(id) => ProviderStructHead::Named(Type::new_struct(id)),
            StructLiteralHead::Absent => ProviderStructHead::Absent,
        }
    }

    /// Run the provider-generic [`is_accessible`] over this driver's registered
    /// file paths — the visibility short-circuit answered from the request-local
    /// path facts, byte-identical to the epoch when the same paths are registered.
    pub fn is_accessible(&self, accessing: FileId, defining: FileId, is_public: bool) -> bool {
        is_accessible(self, accessing, defining, is_public)
    }

    /// Read the body-local minted [`TypeInternPool`] under a closure, so a
    /// differential renders a resolved pool [`Type`] index-independently (the pool
    /// mints its own ids; parity is asserted through displays / metadata, never a
    /// pool-relative index — the 2a contract).
    pub fn with_type_pool<R>(&self, read: impl FnOnce(&TypeInternPool) -> R) -> R {
        let pool = self.identity.type_pool();
        read(&pool)
    }
}

impl<K, M, S> AggregateFacts for ProviderAggregateFacts<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Clone + Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    fn aggregate_value_const(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.value_consts
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    fn aggregate_module_binding(&self, file: FileId, name: Spur) -> Option<ConstInfo> {
        self.module_bindings
            .borrow()
            .get(&(file.index(), name))
            .cloned()
    }

    fn aggregate_struct_in_file(&self, file: FileId, name: Spur) -> Option<StructId> {
        let key = self.by_file_name.get(&(file.index(), name)).cloned()?;
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_struct()
    }

    fn aggregate_enum_in_file(&self, file: FileId, name: Spur) -> Option<EnumId> {
        let key = self.by_file_name.get(&(file.index(), name)).cloned()?;
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::Nominal(key))
            .ok()?
            .as_enum()
    }

    fn aggregate_primitive_type(&self, name: Spur) -> Option<Type> {
        Type::from_primitive_name(self.identity.pool().resolve_symbol(name))
    }

    fn aggregate_builtin_struct(&self, name: Spur) -> Option<StructId> {
        let name = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: std::sync::Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Struct,
            })
            .ok()?
            .as_struct()
    }

    fn aggregate_builtin_enum(&self, name: Spur) -> Option<EnumId> {
        let name = self.identity.pool().resolve_symbol(name).to_owned();
        self.identity
            .pool_mut()?
            .resolve(&SemanticImportType::BuiltinNominal {
                name: std::sync::Arc::from(name.as_str()),
                kind: SemanticImportNominalKind::Enum,
            })
            .ok()?
            .as_enum()
    }

    fn aggregate_module(&self, module: ModuleId) -> AggregateModuleFact {
        let definition = self
            .identity
            .modules()
            .get(module)
            .expect("provider module id must be registered before aggregate resolution");
        AggregateModuleFact {
            file: definition.file_id,
            file_path: definition.file_path,
            import_path: definition.import_path,
        }
    }

    fn aggregate_file_path(&self, file: FileId) -> Option<&str> {
        self.file_paths.get(&file).map(String::as_str)
    }

    fn aggregate_source_path(&self, _span: rue_span::Span) -> Option<&str> {
        // Deferred to the flip: consumed only by inline `aggregates.rs`
        // diagnostic paths, never by the provider-generic selection logic.
        None
    }
}
