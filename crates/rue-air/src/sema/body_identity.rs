//! Body-scoped nominal / type-identity pool for the provider-driven analyzer
//! (RUE-1091 slice r4a-2a).
//!
//! [`BodyIdentityPool`] is the value/type-identity analog of the r2 durable
//! metadata store, promoted to an id-minting pool. Where the epoch pre-registers
//! every source nominal into a [`TypeInternPool`] during declaration gathering
//! and the analyzer merely *looks them up*
//! (`body_endpoint::resolve_instance_type`), the provider-driven analyzer starts
//! from a bare body-local pool and must *mint* each consulted nominal from its
//! durable metadata on first consult, deduplicating on repeat.
//!
//! This slice builds that pool machinery — the same registration path the epoch
//! performs (`declare_struct`/`complete_declared_struct`,
//! `register_enum`/`declare_enum`, `try_intern_array`/`try_intern_ptr_*`,
//! `set_symbol_paths`, `set_struct_lang_item`) — so every downstream read
//! (`struct_def`/`enum_def` metadata, `format_type_name`, `is_type_copy`,
//! `struct_symbol_name`) is byte-equivalent to an epoch-registered twin. The
//! pool mints **internally-consistent** ids carrying correct durable metadata,
//! not epoch-equal numbering: published artifacts are durable-keyed at export
//! (`semantic_body_export.rs`), so the transient pool indices need not match the
//! epoch's (RUE-1091 pool-keystone).
//!
//! Scope of r4a-2a is the nominal / type-identity family: the arms
//! `resolve_instance_type` needs for its primitive, builtin-nominal, named
//! nominal, anonymous, and structural (array / ptr / slice) shapes.
//!
//! Slice r4a-2b extends this with the **callable-identity family**: the pool
//! assembles the signature-derived subset of `FunctionInfo`/`MethodInfo` and
//! the `ParamRange`s they carry from the durable signature vocabulary
//! (`DurableFunction`/`DurableMethod` over `DurableSignatureParameter`), minting
//! its parameters into a pool-owned [`ParamArena`] (the analog of
//! `Sema::param_arena`, which lives *beside* the type pool, not inside it) and
//! resolving every parameter/return/receiver type through the same 2a `resolve`
//! machinery. The request/RIR-carried remainder of each info struct is *not*
//! minted: it is supplied by a caller-provided handle
//! ([`FunctionIdentityHandle`]/[`MethodIdentityHandle`]) — the honest 2b/2c
//! boundary. Every info field is therefore either purely durable-signature-
//! derived (minted here) or purely request/RIR-carried (handle), never both:
//!
//! - **durable (2b):** parameters (name/type/mode/comptime), return type,
//!   `is_generic` (derived: any comptime param), `is_pub`, `is_unchecked`,
//!   `has_self`, and a method's `struct_type` (the 2a-resolved receiver);
//! - **request/RIR (handle):** `body`/`declaration` (RIR `InstRef`s), `span`,
//!   `return_type_sym` (pre-resolution RIR symbol), `is_extern`, `is_c_export`
//!   (an epoch RIR read, not a durable-shell fact), the three `@allow` flags
//!   (RIR directives), `file_id`, and a method's `self_mode`/`self_is_mut`.
//!
//! Deliberately still out of the pool:
//!
//! - the RIR-index answers the endpoint seam consumes (2c): the pool takes the
//!   `body`/`declaration` handles as caller-provided inputs and does not itself
//!   reverse `first_free_function`/`destructors`/`named_method_declarations`;
//! - callables whose parameter/return/receiver types are themselves a *deferred*
//!   2a arm (generic-parameter substitution, module identity) — these are
//!   refused (poisoning the callable key), never approximated, exactly as the
//!   underlying `resolve` refuses them. A comptime-*value* parameter (concrete
//!   type, `is_comptime = true`) resolves fully and marks `is_generic`; only a
//!   comptime-*type* parameter referencing a generic parameter refuses;
//! - **anonymous nominal minting** — an issued anonymous nominal is resolved by
//!   lookup here (exactly as `resolve_instance_type`'s anonymous arm looks up an
//!   already-issued id via `anon_struct`/`anon_enum`); the id-minting from a
//!   structural digest, together with the well-known `Option` facts, is later
//!   work (r6);
//! - **module identity** and generic-parameter substitution (endpoint /
//!   inference families) — these arms are *refused*, never approximated;
//! - **drop metadata** — the destructor symbol and the transitive
//!   linearity / needs-drop finalization. The 2a consumers (display,
//!   copyability, field lookup) never read it, so the pool registers each
//!   nominal with `destructor: None` and leaves the declaration-time
//!   linearity flag un-finalized, exactly as the epoch's shell/completion pair
//!   leaves it before `finalize_containment_metadata`. A consumer needing
//!   finalized needs-drop / transitive linearity (the drop/ownership family)
//!   requires a pool-side `freeze()`-equivalent hook — that seam belongs to
//!   the slice that wires the pool under body analysis (r4b), which must call
//!   `finalize_containment_metadata` at the same point production freezes.

#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use lasso::{Spur, ThreadedRodeo};
use rue_rir::{InstData, InstRef, Rir, RirParamMode};
use rue_span::{FileId, Span};

use super::declaration_index::{RirDeclarationIndex, RirDestructorDeclaration};
use super::info::{FunctionInfo, MethodInfo};
use crate::types::{EnumDef, EnumId, LangItem, StructDef, StructField, StructId, Type};
use crate::{
    AnonymousNominalKey, ParamArena, ParamRange, SemanticImportNominalKind, SemanticImportType,
    SemanticParameterMode, TypeInternPool,
};

/// The durable body of a named nominal: its field / variant vocabulary plus the
/// declaration-time metadata the pool registration consumes.
///
/// Drop metadata (the destructor symbol, transitive linearity) is intentionally
/// absent — see the module docs. The declaration-time `is_linear` flag is
/// carried because the epoch's declaration shell carries it verbatim (it is a
/// durable field of `DurableDeclarationPayload::Struct`); the pool does not
/// finalize the transitive linearity join.
#[derive(Debug, Clone)]
pub(in crate::sema) enum DurableNominalBody<K, M> {
    Struct {
        /// Fields in declaration order: source name and durable field type.
        fields: Vec<(Arc<str>, SemanticImportType<K, M>)>,
        is_copy: bool,
        is_linear: bool,
    },
    Enum {
        /// Variants in declaration order: source name and durable payload types.
        variants: Vec<(Arc<str>, Vec<SemanticImportType<K, M>>)>,
    },
}

/// Durable metadata for one named nominal, sufficient to register a
/// byte-equivalent pool entry: everything the epoch's `StructDef`/`EnumDef`
/// registration needs for the 2a consumers.
#[derive(Debug, Clone)]
pub(in crate::sema) struct DurableNominal<K, M> {
    pub name: Arc<str>,
    /// The nominal's defining module logical path. Assigned a body-local
    /// [`FileId`] and published to the pool's symbol paths so nominal symbol
    /// qualification mangles the same module component the epoch does.
    pub module_path: Arc<str>,
    pub is_public: bool,
    pub is_builtin: bool,
    pub lang_item: Option<LangItem>,
    /// `@repr(c)` — a declaration-time side fact the epoch's shell phase sets
    /// (`set_struct_repr_c`). Carried so the pool registers it rather than
    /// silently dropping a declaration fact; struct-only (ignored for enums).
    pub is_repr_c: bool,
    pub body: DurableNominalBody<K, M>,
}

/// The durable nominal vocabulary the pool consults to mint a named nominal.
/// Implemented by the r4b provider side (over r2's stable-keyed metadata) and by
/// the 2a unit tests.
pub(in crate::sema) trait DurableNominalSource<K, M> {
    /// The durable metadata for a nominal key, or `None` if the key names no
    /// nominal in the durable universe.
    fn nominal(&self, key: &K) -> Option<DurableNominal<K, M>>;
}

/// Why the pool could not mint an identity for a durable type. Every arm is a
/// closed refusal — the pool never approximates an identity it cannot mint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::sema) enum IdentityMintError {
    /// A named nominal key resolves to no durable metadata.
    MissingNominal,
    /// A callable (function / method) key resolves to no durable signature.
    MissingCallable,
    /// An anonymous nominal key was consulted before its issued id was
    /// registered with the pool.
    MissingAnonymous,
    /// A builtin nominal name is not one of the pre-registered builtins.
    UnknownBuiltinNominal,
    /// A builtin nominal name exists but under the other nominal kind.
    BuiltinNominalKindMismatch,
    /// A structural wrap (array / pointer) failed pool validation.
    InvalidStructuralType,
    /// An arm outside slice r4a-2a's scope (module identity, generic parameter).
    Deferred(&'static str),
}

#[derive(Clone, Copy)]
enum PoolNominal {
    Struct(StructId),
    Enum(EnumId),
}

/// A body-scoped id-minting nominal / type pool.
///
/// Owns a fresh [`TypeInternPool`] and interner. Builtin enums and the core
/// `str` identity are pre-registered exactly as a fresh import epoch registers
/// them (`SemanticImportedProgram::new`), so the builtin-nominal arm resolves.
/// Named nominals are minted on first [`resolve`](Self::resolve) and
/// deduplicated by durable key thereafter.
pub(in crate::sema) struct BodyIdentityPool<K, M, S> {
    type_pool: TypeInternPool,
    interner: ThreadedRodeo,
    source: S,
    struct_ids: HashMap<K, StructId>,
    enum_ids: HashMap<K, EnumId>,
    /// Keys whose mint failed after shell registration; repeat consults
    /// re-error rather than exposing the incomplete shell (see `mint_named`).
    poisoned: HashMap<K, IdentityMintError>,
    anon_nominals: HashMap<AnonymousNominalKey<K, M>, Type>,
    builtins: HashMap<(Arc<str>, SemanticImportNominalKind), PoolNominal>,
    module_files: HashMap<Arc<str>, FileId>,
    /// The pool's own parameter arena, the analog of `Sema::param_arena` (which
    /// lives *beside* the type pool, not inside it). Callable identities intern
    /// their durable parameter vocabulary here on first consult, returning a
    /// `ParamRange` that indexes this arena — never the epoch's.
    param_arena: ParamArena,
    /// Minted function signatures, deduplicated by durable callable key: the
    /// arena is append-only, so a repeat consult must return the cached
    /// `ParamRange` rather than re-interning the same parameters.
    function_sigs: HashMap<K, CallableSignature>,
    /// Minted method signatures, deduplicated by durable callable key.
    method_sigs: HashMap<K, MethodSignature>,
    /// Callable keys whose signature mint failed. A repeat consult re-errors
    /// rather than re-running the partial mint (whose parameters may already sit
    /// orphaned in the append-only arena) — the callable analog of `poisoned`.
    callable_poisoned: HashMap<K, IdentityMintError>,
}

/// The durable-signature-derived subset of a [`FunctionInfo`], minted once and
/// cached by callable key. Every field here is recoverable from the durable
/// signature facts alone; the request/RIR-carried remainder (`body`,
/// `declaration`, `span`, `return_type_sym`, `is_extern`, `is_c_export`, the
/// `@allow` flags, `file_id`) is supplied by a [`FunctionIdentityHandle`] — the
/// 2c / request-local seam.
#[derive(Clone, Copy)]
struct CallableSignature {
    params: ParamRange,
    return_type: Type,
    is_generic: bool,
    is_pub: bool,
    is_unchecked: bool,
}

/// The durable-signature-derived subset of a [`MethodInfo`], minted once and
/// cached by callable key. `struct_type`, `has_self`, `params`, and
/// `return_type` are durable; `self_mode`, `self_is_mut`, `body`, and `span`
/// are request/RIR-carried and arrive on a [`MethodIdentityHandle`].
#[derive(Clone, Copy)]
struct MethodSignature {
    receiver: Type,
    has_self: bool,
    params: ParamRange,
    return_type: Type,
}

impl<K, M, S> BodyIdentityPool<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M>,
{
    /// Create an empty pool with the builtin enums and the core `str` identity
    /// pre-registered, mirroring a fresh import epoch.
    pub(in crate::sema) fn new(source: S) -> Self {
        let type_pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let mut builtins = HashMap::new();

        for builtin in rue_builtins::BUILTIN_ENUMS {
            let symbol = interner.get_or_intern(builtin.name);
            let (id, _) = type_pool.register_enum(
                symbol,
                EnumDef {
                    name: builtin.name.to_owned(),
                    variants: builtin.variants.iter().map(|v| (*v).to_owned()).collect(),
                    variant_payloads: Vec::new(),
                    is_pub: true,
                    file_id: FileId::DEFAULT,
                },
            );
            builtins.insert(
                (Arc::from(builtin.name), SemanticImportNominalKind::Enum),
                PoolNominal::Enum(id),
            );
        }

        // The core `str` identity: an ordinary builtin struct paired with a
        // runtime definition. Registered exactly as `SemanticImportedProgram::new`
        // registers it.
        let str_symbol = interner.get_or_intern("str");
        let ptr_id = type_pool.intern_ptr_const_from_type(Type::U8);
        let (str_id, _) = type_pool.register_struct(
            str_symbol,
            StructDef {
                name: "str".to_owned(),
                fields: vec![
                    StructField {
                        name: "ptr".to_owned(),
                        ty: Type::new_ptr_const(ptr_id),
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
                file_id: FileId::DEFAULT,
            },
        );
        builtins.insert(
            (Arc::from("str"), SemanticImportNominalKind::Struct),
            PoolNominal::Struct(str_id),
        );

        Self {
            type_pool,
            interner,
            source,
            struct_ids: HashMap::new(),
            enum_ids: HashMap::new(),
            poisoned: HashMap::new(),
            anon_nominals: HashMap::new(),
            builtins,
            module_files: HashMap::new(),
            param_arena: ParamArena::new(),
            function_sigs: HashMap::new(),
            method_sigs: HashMap::new(),
            callable_poisoned: HashMap::new(),
        }
    }

    /// The body-local pool. Downstream reads (`struct_def`, `enum_def`,
    /// `struct_symbol_name`, layout, copyability) go through this handle exactly
    /// as the analyzer reads `sema.type_pool`.
    pub(in crate::sema) fn type_pool(&self) -> &TypeInternPool {
        &self.type_pool
    }

    /// The body-local parameter arena. Callable identities' [`ParamRange`]s index
    /// this arena, read exactly as the analyzer reads `sema.param_arena`.
    pub(in crate::sema) fn param_arena(&self) -> &ParamArena {
        &self.param_arena
    }

    /// Resolve an interned parameter/name symbol to its source string. The
    /// pool's [`Spur`]s are pool-interner-relative (like its pool indices), so
    /// name parity is asserted through resolved strings, never raw symbols.
    pub(in crate::sema) fn resolve_symbol(&self, symbol: Spur) -> &str {
        self.interner.resolve(&symbol)
    }

    /// Record the concrete [`Type`] an anonymous nominal was issued, so a later
    /// consult of its key resolves by lookup. Mirrors the epoch's
    /// `anon_struct_identities` / `anon_enum_identities` maps that
    /// `resolve_instance_type` consults for the anonymous arm.
    pub(in crate::sema) fn register_issued_anonymous(
        &mut self,
        key: AnonymousNominalKey<K, M>,
        ty: Type,
    ) {
        self.anon_nominals.insert(key, ty);
    }

    /// Mint (on first consult) or dedup a concrete [`Type`] for a durable type.
    ///
    /// The provider-driven analog of `body_endpoint::resolve_instance_type`: a
    /// direct recursive walk of the durable type algebra whose nominal arm mints
    /// and registers rather than looks up.
    pub(in crate::sema) fn resolve(
        &mut self,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        use SemanticImportType as S;
        Ok(match value {
            S::I8 => Type::I8,
            S::I16 => Type::I16,
            S::I32 => Type::I32,
            S::I64 => Type::I64,
            S::U8 => Type::U8,
            S::U16 => Type::U16,
            S::U32 => Type::U32,
            S::U64 => Type::U64,
            S::Bool => Type::BOOL,
            S::Unit => Type::UNIT,
            S::Never => Type::NEVER,
            S::ComptimeType => Type::COMPTIME_TYPE,
            S::BuiltinNominal { name, kind } => {
                match self.builtins.get(&(name.clone(), *kind)).copied() {
                    Some(PoolNominal::Struct(id)) => Type::new_struct(id),
                    Some(PoolNominal::Enum(id)) => Type::new_enum(id),
                    None => {
                        return Err(if self.builtins.keys().any(|(known, _)| known == name) {
                            IdentityMintError::BuiltinNominalKindMismatch
                        } else {
                            IdentityMintError::UnknownBuiltinNominal
                        });
                    }
                }
            }
            S::Nominal(key) => self.mint_named(key)?,
            S::AnonymousNominal(key) => self
                .anon_nominals
                .get(key)
                .copied()
                .ok_or(IdentityMintError::MissingAnonymous)?,
            S::Array { element, len } => {
                let element = self.resolve(element)?;
                self.type_pool
                    .try_intern_array(element, *len)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)?
            }
            S::PtrConst(inner) => {
                let pointee = self.resolve(inner)?;
                self.type_pool
                    .try_intern_ptr_const(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)?
            }
            S::PtrMut(inner) => {
                let pointee = self.resolve(inner)?;
                self.type_pool
                    .try_intern_ptr_mut(pointee)
                    .map_err(|_| IdentityMintError::InvalidStructuralType)?
            }
            S::Slice { element, name } => {
                // A slice view is a generated fat-pointer struct. Registered
                // exactly as `SemanticImportedProgram::import_type_local`
                // registers it (ptr + len, builtin, copy).
                let element = self.resolve(element)?;
                let symbol = self.interner.get_or_intern(name.as_ref());
                let pointer = self.type_pool.intern_ptr_const_from_type(element);
                let (id, _) = self.type_pool.register_struct(
                    symbol,
                    StructDef {
                        name: name.to_string(),
                        fields: vec![
                            StructField {
                                name: "ptr".to_owned(),
                                ty: Type::new_ptr_const(pointer),
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
                        file_id: FileId::DEFAULT,
                    },
                );
                Type::new_struct(id)
            }
            S::Module(_) => return Err(IdentityMintError::Deferred("module identity")),
            S::GenericParameter(_) => {
                return Err(IdentityMintError::Deferred("generic parameter"));
            }
        })
    }

    /// Mint or dedup a named nominal.
    ///
    /// On first consult the shell is registered and inserted into the dedup map
    /// **before** its field / payload types are resolved, so a nominal that
    /// refers to itself (through a pointer) resolves the recursive reference to
    /// the shell id — the epoch's declare-then-complete discipline.
    fn mint_named(&mut self, key: &K) -> Result<Type, IdentityMintError> {
        // A failed mint leaves an incomplete shell in the intern pool (the
        // shell must pre-register for recursive self-reference, and the pool
        // is append-only, so it cannot be rolled back). The poison map keeps
        // that shell unreachable: a repeat consult re-errors instead of
        // handing out an id whose `struct_def` read would panic.
        if let Some(err) = self.poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&id) = self.struct_ids.get(key) {
            return Ok(Type::new_struct(id));
        }
        if let Some(&id) = self.enum_ids.get(key) {
            return Ok(Type::new_enum(id));
        }

        let DurableNominal {
            name,
            module_path,
            is_public,
            is_builtin,
            lang_item,
            is_repr_c,
            body,
        } = self
            .source
            .nominal(key)
            .ok_or(IdentityMintError::MissingNominal)?;

        let file_id = self.file_for_module(&module_path);
        let symbol = self.interner.get_or_intern(name.as_ref());
        let name = name.to_string();

        match body {
            DurableNominalBody::Struct {
                fields,
                is_copy,
                is_linear,
            } => {
                let (id, _) = self.type_pool.declare_struct(
                    symbol,
                    StructDef {
                        name: name.clone(),
                        fields: Vec::new(),
                        is_copy,
                        is_linear,
                        destructor: None,
                        is_builtin,
                        is_pub: is_public,
                        file_id,
                    },
                );
                self.struct_ids.insert(key.clone(), id);
                if let Some(lang_item) = lang_item {
                    self.type_pool.set_struct_lang_item(id, lang_item);
                }
                if is_repr_c {
                    self.type_pool.set_struct_repr_c(id);
                }

                let mut resolved = Vec::with_capacity(fields.len());
                for (field_name, field_ty) in &fields {
                    let ty = match self.resolve(field_ty) {
                        Ok(ty) => ty,
                        Err(err) => {
                            self.poisoned.insert(key.clone(), err.clone());
                            return Err(err);
                        }
                    };
                    resolved.push(StructField {
                        name: field_name.to_string(),
                        ty,
                    });
                }
                self.type_pool.complete_declared_struct(
                    id,
                    StructDef {
                        name,
                        fields: resolved,
                        is_copy,
                        is_linear,
                        destructor: None,
                        is_builtin,
                        is_pub: is_public,
                        file_id,
                    },
                );
                Ok(Type::new_struct(id))
            }
            DurableNominalBody::Enum { variants } => {
                let variant_names: Vec<String> =
                    variants.iter().map(|(name, _)| name.to_string()).collect();
                let (id, _) = self.type_pool.declare_enum(
                    symbol,
                    EnumDef {
                        name: name.clone(),
                        variants: variant_names.clone(),
                        variant_payloads: Vec::new(),
                        is_pub: is_public,
                        file_id,
                    },
                );
                self.enum_ids.insert(key.clone(), id);

                let mut variant_payloads = Vec::with_capacity(variants.len());
                for (_, payload) in &variants {
                    let mut resolved = Vec::with_capacity(payload.len());
                    for ty in payload {
                        match self.resolve(ty) {
                            Ok(ty) => resolved.push(ty),
                            Err(err) => {
                                self.poisoned.insert(key.clone(), err.clone());
                                return Err(err);
                            }
                        }
                    }
                    variant_payloads.push(resolved);
                }
                self.type_pool.complete_declared_enum(
                    id,
                    EnumDef {
                        name,
                        variants: variant_names,
                        variant_payloads,
                        is_pub: is_public,
                        file_id,
                    },
                );
                Ok(Type::new_enum(id))
            }
        }
    }

    /// The body-local [`FileId`] for a module logical path, assigned on first
    /// sight. Re-publishes the accumulated logical paths so `struct_symbol_name`
    /// mangles the same module component the epoch does. The numbering is
    /// body-internal; only the path string a `FileId` maps to is load-bearing
    /// for display parity.
    fn file_for_module(&mut self, module_path: &Arc<str>) -> FileId {
        if let Some(&file) = self.module_files.get(module_path) {
            return file;
        }
        let next = u32::try_from(self.module_files.len() + 1).expect("too many body modules");
        let file = FileId::new(next);
        self.module_files.insert(module_path.clone(), file);
        self.type_pool.set_symbol_paths(
            self.module_files
                .iter()
                .map(|(path, id)| (*id, path.to_string()))
                .collect(),
        );
        file
    }
}

/// One durable parameter of a callable signature: the r5a durable parameter
/// vocabulary (`DurableSemanticParameter { name, ty, mode, is_comptime }`)
/// projected into rue-air's durable type algebra. `name` is carried durably
/// (r5a) so the pool assembles a `ParamRange` whose names match the epoch's
/// without re-consulting a declaration shell.
#[derive(Debug, Clone)]
pub(in crate::sema) struct DurableSignatureParameter<K, M> {
    pub name: Arc<str>,
    pub ty: SemanticImportType<K, M>,
    pub mode: SemanticParameterMode,
    pub is_comptime: bool,
}

/// The durable signature of a free function, sufficient to assemble the
/// signature-derived subset of a [`FunctionInfo`]. `is_generic` is *not* a field
/// — it is the derived predicate "any parameter is comptime", exactly the
/// invariant the epoch enforces between its shell and RIR
/// (`binding_manifest.rs`: `shell.is_generic == params.any(is_comptime)`).
#[derive(Debug, Clone)]
pub(in crate::sema) struct DurableFunction<K, M> {
    pub parameters: Vec<DurableSignatureParameter<K, M>>,
    pub result: SemanticImportType<K, M>,
    pub is_public: bool,
    pub is_unchecked: bool,
}

/// The durable signature of a method. The `receiver` is a durable type (the
/// concrete owning nominal — the epoch's `Self` is pre-resolved at export, so
/// no `Self` substitution is needed here) resolved through the same 2a nominal
/// machinery as the parameters.
#[derive(Debug, Clone)]
pub(in crate::sema) struct DurableMethod<K, M> {
    pub receiver: SemanticImportType<K, M>,
    pub parameters: Vec<DurableSignatureParameter<K, M>>,
    pub result: SemanticImportType<K, M>,
    pub has_self: bool,
}

/// The durable callable vocabulary the pool consults to mint callable
/// identities. Implemented by the r4b provider side (over r2's stable-keyed
/// metadata) and by the 2b unit tests. Keys are namespace-disjoint from nominal
/// keys in the durable universe (`StableDefinitionKey` encodes namespace+kind),
/// so a key names at most one of a nominal, a function, or a method.
pub(in crate::sema) trait DurableCallableSource<K, M> {
    /// The durable signature for a free-function key, or `None` if the key names
    /// no function in the durable universe.
    fn function(&self, key: &K) -> Option<DurableFunction<K, M>>;
    /// The durable signature for a method key, or `None` if the key names no
    /// method in the durable universe.
    fn method(&self, key: &K) -> Option<DurableMethod<K, M>>;
}

/// The request/RIR-carried facts a caller supplies to assemble a
/// [`FunctionInfo`]: everything the durable signature does *not* carry.
///
/// These are the 2c / request-local seam. The durable facts stop at the
/// signature (spans and RIR handles belong to an exact semantic request —
/// `semantic_import.rs`); `is_c_export` is read from the current RIR by the
/// epoch itself (`binding_manifest.rs`), never threaded through the durable
/// shell; `return_type_sym` is a pre-resolution RIR symbol consumed only by
/// generic specialization / export; the `@allow` flags come from RIR
/// directives. Supplying them here (rather than fabricating them) is the honest
/// boundary — the pool refuses to invent a fact the durable universe lacks.
#[derive(Debug, Clone, Copy)]
pub(in crate::sema) struct FunctionIdentityHandle {
    pub body: InstRef,
    pub declaration: InstRef,
    pub span: Span,
    pub return_type_sym: Spur,
    pub is_extern: bool,
    pub is_c_export: bool,
    pub allow_unused_function: bool,
    pub allow_unused_variable: bool,
    pub allow_unreachable_code: bool,
    pub file_id: FileId,
}

/// The request/RIR-carried facts a caller supplies to assemble a [`MethodInfo`].
/// `self_mode` / `self_is_mut` are RIR `FnDecl` facts the durable `Callable`
/// payload omits (it carries only `has_self`); `self_is_mut` is additionally
/// body-local (call sites ignore it). `body` and `span` are request-carried.
#[derive(Debug, Clone, Copy)]
pub(in crate::sema) struct MethodIdentityHandle {
    pub body: InstRef,
    pub span: Span,
    pub self_mode: RirParamMode,
    pub self_is_mut: bool,
}

impl<K, M, S> BodyIdentityPool<K, M, S>
where
    K: Clone + Eq + Hash,
    M: Eq + Hash,
    S: DurableNominalSource<K, M> + DurableCallableSource<K, M>,
{
    /// Assemble a [`FunctionInfo`] for a durable function key, combining the
    /// minted signature-derived subset with the caller-provided request/RIR
    /// facts. Mints the signature on first consult; deduplicates thereafter.
    pub(in crate::sema) fn resolve_function(
        &mut self,
        key: &K,
        handle: FunctionIdentityHandle,
    ) -> Result<FunctionInfo, IdentityMintError> {
        let signature = self.function_signature(key)?;
        Ok(FunctionInfo {
            params: signature.params,
            return_type: signature.return_type,
            return_type_sym: handle.return_type_sym,
            body: handle.body,
            declaration: handle.declaration,
            span: handle.span,
            is_generic: signature.is_generic,
            is_pub: signature.is_pub,
            is_unchecked: signature.is_unchecked,
            is_extern: handle.is_extern,
            is_c_export: handle.is_c_export,
            allow_unused_function: handle.allow_unused_function,
            allow_unused_variable: handle.allow_unused_variable,
            allow_unreachable_code: handle.allow_unreachable_code,
            file_id: handle.file_id,
        })
    }

    /// Assemble a [`MethodInfo`] for a durable method key, combining the minted
    /// signature-derived subset with the caller-provided request/RIR facts.
    pub(in crate::sema) fn resolve_method(
        &mut self,
        key: &K,
        handle: MethodIdentityHandle,
    ) -> Result<MethodInfo, IdentityMintError> {
        let signature = self.method_signature(key)?;
        Ok(MethodInfo {
            struct_type: signature.receiver,
            has_self: signature.has_self,
            self_mode: handle.self_mode,
            self_is_mut: handle.self_is_mut,
            params: signature.params,
            return_type: signature.return_type,
            body: handle.body,
            span: handle.span,
        })
    }

    /// Mint (once) or dedup the signature-derived subset of a function.
    fn function_signature(&mut self, key: &K) -> Result<CallableSignature, IdentityMintError> {
        if let Some(err) = self.callable_poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&signature) = self.function_sigs.get(key) {
            return Ok(signature);
        }

        let DurableFunction {
            parameters,
            result,
            is_public,
            is_unchecked,
        } = self
            .source
            .function(key)
            .ok_or(IdentityMintError::MissingCallable)?;

        let is_generic = parameters.iter().any(|parameter| parameter.is_comptime);
        // Parameters intern first (mirroring the epoch, which allocs the arena
        // before resolving the return type); a failure at either step poisons
        // the key so the append-only arena's orphaned parameters stay
        // unreachable and the repeat consult re-errors.
        let params = self.intern_params(key, &parameters)?;
        let return_type = self.resolve_callable_type(key, &result)?;

        let signature = CallableSignature {
            params,
            return_type,
            is_generic,
            is_pub: is_public,
            is_unchecked,
        };
        self.function_sigs.insert(key.clone(), signature);
        Ok(signature)
    }

    /// Mint (once) or dedup the signature-derived subset of a method.
    fn method_signature(&mut self, key: &K) -> Result<MethodSignature, IdentityMintError> {
        if let Some(err) = self.callable_poisoned.get(key) {
            return Err(err.clone());
        }
        if let Some(&signature) = self.method_sigs.get(key) {
            return Ok(signature);
        }

        let DurableMethod {
            receiver,
            parameters,
            result,
            has_self,
        } = self
            .source
            .method(key)
            .ok_or(IdentityMintError::MissingCallable)?;

        let receiver = self.resolve_callable_type(key, &receiver)?;
        let params = self.intern_params(key, &parameters)?;
        let return_type = self.resolve_callable_type(key, &result)?;

        let signature = MethodSignature {
            receiver,
            has_self,
            params,
            return_type,
        };
        self.method_sigs.insert(key.clone(), signature);
        Ok(signature)
    }

    /// Resolve one durable type inside a callable signature, poisoning the
    /// callable key on failure (so a partial mint never re-runs).
    fn resolve_callable_type(
        &mut self,
        key: &K,
        value: &SemanticImportType<K, M>,
    ) -> Result<Type, IdentityMintError> {
        self.resolve(value).inspect_err(|err| {
            self.callable_poisoned.insert(key.clone(), (*err).clone());
        })
    }

    /// Intern a durable parameter vocabulary into the pool's own arena, returning
    /// an internally-consistent [`ParamRange`]. Types resolve through the 2a
    /// nominal machinery (`resolve`); names intern into the pool's interner;
    /// modes map to the RIR mode the arena stores; comptime flags copy through.
    fn intern_params(
        &mut self,
        key: &K,
        parameters: &[DurableSignatureParameter<K, M>],
    ) -> Result<ParamRange, IdentityMintError> {
        let mut names = Vec::with_capacity(parameters.len());
        let mut types = Vec::with_capacity(parameters.len());
        let mut modes = Vec::with_capacity(parameters.len());
        let mut comptime = Vec::with_capacity(parameters.len());
        for parameter in parameters {
            // Resolve the type first: on failure the arena is untouched (alloc is
            // the final step), so only the callable key is poisoned.
            let ty = self.resolve_callable_type(key, &parameter.ty)?;
            names.push(self.interner.get_or_intern(parameter.name.as_ref()));
            types.push(ty);
            modes.push(match parameter.mode {
                SemanticParameterMode::Value => RirParamMode::Normal,
                SemanticParameterMode::Borrow => RirParamMode::Borrow,
                SemanticParameterMode::Inout => RirParamMode::Inout,
            });
            comptime.push(parameter.is_comptime);
        }
        Ok(self.param_arena.alloc(names, types, modes, comptime))
    }
}

/// The body-scoped RIR answer surface for the endpoint seam (slice r4a-2c): the
/// provider-side equivalent of the three endpoint ops `one_body.rs` consumes
/// through [`super::body_endpoint::EpochFacts`] — `first_free_function`,
/// `named_method_declaration`, and `destructor`.
///
/// # The shared-`Rir` input, not durable state
///
/// Every answer is an [`InstRef`] into the whole-program `rir: &Rir` this index
/// was built from — a body-query *input*, never durable. The design checkpoint
/// fixed this: "Projection is a metadata join and must never inspect RIR"; the
/// RIR index is the *other* leg, the query-input side that resolves durable
/// declaration identities into the current arena's instruction handles. The
/// returned handles are valid only against that exact `Rir`, exactly as
/// [`RirDeclarationIndex`]'s are, so a caller composes them with the same `Rir`
/// its [`FunctionIdentityHandle`] / [`MethodIdentityHandle`] index into.
///
/// # New-vs-reuse: a thin façade over the body-independent production index
///
/// [`RirDeclarationIndex`] is already body-independent — `RirDeclarationIndex::
/// new(rir)` is built from the shared `Rir` alone, its `InstRef`s locate that
/// arena, and it already answers `first_free_function` and `destructors`
/// verbatim (they are the tables [`super::body_endpoint::EpochFacts`] delegates
/// to). So this index **re-uses it wholesale** for those two ops rather than
/// duplicating the arena walk.
///
/// The one op the production index does not expose as a keyed point lookup is
/// `named_method_declaration`: the epoch answers it from the *`Sema`-side*
/// `named_method_declarations` map keyed by a pool-minted [`StructId`]
/// (`declarations.rs`), which the pool-free RIR index cannot hold. But that
/// `StructId` is only an *intermediate* the epoch computes from `struct_by_file_
/// name(file, type_name)` — a bijection (duplicate `(file, name)` is E0418), so
/// its durable-available preimage `(owner_file, owner_type_name)` is an exact
/// substitute key. This index therefore re-keys the owner→method edges
/// [`RirDeclarationIndex`] already collected (retained on `shell_declarations`'
/// `named_method_owner`) into a `(FileId, type_name, method_name) → InstRef`
/// point map — the sole additive surface. No keying input is pool-minted; every
/// key is RIR-derivable from the shared `Rir`.
///
/// Inert per the pool arc: `#![cfg_attr(not(test), allow(dead_code))]` on this
/// module. There are zero production callers; the flip slice (r4b) wires it
/// under body analysis.
pub(in crate::sema) struct BodyRirIndex {
    /// Production's body-independent declaration index, re-used verbatim for
    /// `first_free_function` and `destructor`.
    declarations: RirDeclarationIndex,
    /// The one additive keyed surface: named-method declarations keyed by the
    /// durable-available `(owner_file, owner_type_name, method_name)` preimage
    /// of the epoch's `(StructId, method_name)` key.
    named_methods_by_owner: HashMap<(FileId, Spur, Spur), InstRef>,
}

impl BodyRirIndex {
    /// Build the index from the shared whole-program `Rir`. Constructs one
    /// [`RirDeclarationIndex`] and derives the named-method point map from the
    /// owner edges it already classified — no second arena walk of the method
    /// universe, no durable metadata, no pool.
    pub(in crate::sema) fn new(rir: &Rir) -> Self {
        let declarations = RirDeclarationIndex::new(rir);
        let mut named_methods_by_owner = HashMap::new();
        for shell in declarations.shell_declarations() {
            // `named_method_owner` is `Some` exactly for named methods (free
            // functions, nominals, consts, and destructors carry `None`); the
            // owner is the enclosing struct's source-name symbol. A named
            // method's own `span.file_id` is its enclosing struct's file (it is
            // lexically inline), so it is the same file the epoch keys by via
            // `struct_by_file_name(struct_span.file_id, type_name)`.
            let Some(owner) = shell.named_method_owner else {
                continue;
            };
            let inst = rir.get(shell.declaration);
            if let InstData::FnDecl { name, .. } = inst.data {
                // First edge wins, mirroring `RirDeclarationIndex`'s
                // `named_method_owners.or_insert` and the epoch's E0418 rejection
                // of a duplicate `(struct, method)`.
                named_methods_by_owner
                    .entry((inst.span.file_id, owner, name))
                    .or_insert(shell.declaration);
            }
        }
        Self {
            declarations,
            named_methods_by_owner,
        }
    }

    /// The first free-function RIR declaration for `(source, file)`. The exact
    /// answer [`super::body_endpoint::EpochFacts::first_free_function`] returns.
    pub(in crate::sema) fn first_free_function(
        &self,
        source: Spur,
        file_id: FileId,
    ) -> Option<InstRef> {
        self.declarations.first_free_function(source, Some(file_id))
    }

    /// The named-method RIR declaration for a named struct owner, keyed by the
    /// durable-available `(owner_file, owner_type_name, method_name)` preimage of
    /// the epoch's `(StructId, method_name)` key. Equal to
    /// [`super::body_endpoint::EpochFacts::named_method_declaration`] under the
    /// `struct_by_file_name` bijection.
    pub(in crate::sema) fn named_method_declaration(
        &self,
        owner_file: FileId,
        owner_type_name: Spur,
        method_name: Spur,
    ) -> Option<InstRef> {
        self.named_methods_by_owner
            .get(&(owner_file, owner_type_name, method_name))
            .copied()
    }

    /// The destructor declaration record for `(file, type_name)`. The exact
    /// scan [`super::body_endpoint::EpochFacts::destructor`] performs.
    pub(in crate::sema) fn destructor(
        &self,
        file: u32,
        type_name: Spur,
    ) -> Option<RirDestructorDeclaration> {
        self.declarations
            .destructors()
            .iter()
            .find(|record| record.span.file_id.index() == file && record.type_name == type_name)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic_identity::{AnonymousNominalKind, CanonicalArguments, StableProducerId};

    type Key = u32;
    type Module = Arc<str>;
    type DType = SemanticImportType<Key, Module>;

    /// A durable nominal + callable source backed by fixed maps, standing in for
    /// r4b's stable-keyed provider.
    struct MapSource {
        nominals: HashMap<Key, DurableNominal<Key, Module>>,
        functions: HashMap<Key, DurableFunction<Key, Module>>,
        methods: HashMap<Key, DurableMethod<Key, Module>>,
    }

    impl DurableNominalSource<Key, Module> for MapSource {
        fn nominal(&self, key: &Key) -> Option<DurableNominal<Key, Module>> {
            self.nominals.get(key).cloned()
        }
    }

    impl DurableCallableSource<Key, Module> for MapSource {
        fn function(&self, key: &Key) -> Option<DurableFunction<Key, Module>> {
            self.functions.get(key).cloned()
        }

        fn method(&self, key: &Key) -> Option<DurableMethod<Key, Module>> {
            self.methods.get(key).cloned()
        }
    }

    fn source(nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>) -> MapSource {
        MapSource {
            nominals: nominals.into_iter().collect(),
            functions: HashMap::new(),
            methods: HashMap::new(),
        }
    }

    fn pool(
        nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>,
    ) -> BodyIdentityPool<Key, Module, MapSource> {
        BodyIdentityPool::new(source(nominals))
    }

    /// A pool seeded with nominals, functions, and methods for the callable
    /// (2b) tests.
    fn callable_pool(
        nominals: impl IntoIterator<Item = (Key, DurableNominal<Key, Module>)>,
        functions: impl IntoIterator<Item = (Key, DurableFunction<Key, Module>)>,
        methods: impl IntoIterator<Item = (Key, DurableMethod<Key, Module>)>,
    ) -> BodyIdentityPool<Key, Module, MapSource> {
        BodyIdentityPool::new(MapSource {
            nominals: nominals.into_iter().collect(),
            functions: functions.into_iter().collect(),
            methods: methods.into_iter().collect(),
        })
    }

    fn param(
        name: &str,
        ty: DType,
        mode: SemanticParameterMode,
        is_comptime: bool,
    ) -> DurableSignatureParameter<Key, Module> {
        DurableSignatureParameter {
            name: Arc::from(name),
            ty,
            mode,
            is_comptime,
        }
    }

    fn durable_function(
        parameters: Vec<DurableSignatureParameter<Key, Module>>,
        result: DType,
        is_public: bool,
        is_unchecked: bool,
    ) -> DurableFunction<Key, Module> {
        DurableFunction {
            parameters,
            result,
            is_public,
            is_unchecked,
        }
    }

    fn durable_method(
        receiver: DType,
        parameters: Vec<DurableSignatureParameter<Key, Module>>,
        result: DType,
        has_self: bool,
    ) -> DurableMethod<Key, Module> {
        DurableMethod {
            receiver,
            parameters,
            result,
            has_self,
        }
    }

    /// A caller-provided function handle carrying deterministic request/RIR
    /// facts. `resolve_function` must reproduce every field verbatim.
    fn fn_handle(return_type_sym: Spur) -> FunctionIdentityHandle {
        FunctionIdentityHandle {
            body: InstRef::from_raw(101),
            declaration: InstRef::from_raw(102),
            span: Span::with_file(FileId::new(7), 3, 9),
            return_type_sym,
            // Alternate true/false so a hardcoded passthrough of either
            // polarity fails the verbatim-handle assertions.
            is_extern: true,
            is_c_export: false,
            allow_unused_function: true,
            allow_unused_variable: false,
            allow_unreachable_code: true,
            file_id: FileId::new(7),
        }
    }

    fn method_handle() -> MethodIdentityHandle {
        MethodIdentityHandle {
            body: InstRef::from_raw(201),
            span: Span::with_file(FileId::new(3), 1, 4),
            self_mode: RirParamMode::Borrow,
            self_is_mut: true,
        }
    }

    /// Compare a pool `ParamRange` against an epoch-twin one field-column by
    /// field-column, through the same reads the analyzer performs. Names compare
    /// as resolved strings and types via the index-independent `render`/`is_copy`
    /// mirrors, so it is safe across two independently-interned arenas.
    fn assert_param_range_equal(
        pool: &BodyIdentityPool<Key, Module, MapSource>,
        pool_range: ParamRange,
        twin_arena: &ParamArena,
        twin_interner: &ThreadedRodeo,
        twin_range: ParamRange,
        twin_pool: &TypeInternPool,
    ) {
        let arena = pool.param_arena();
        assert_eq!(pool_range.len(), twin_range.len(), "param count");
        for index in 0..pool_range.len() {
            assert_eq!(
                pool.resolve_symbol(arena.names(pool_range)[index]),
                twin_interner.resolve(&twin_arena.names(twin_range)[index]),
                "param name"
            );
            assert_eq!(
                render(pool.type_pool(), arena.types(pool_range)[index]),
                render(twin_pool, twin_arena.types(twin_range)[index]),
                "param type display"
            );
            assert_eq!(
                is_copy(pool.type_pool(), arena.types(pool_range)[index]),
                is_copy(twin_pool, twin_arena.types(twin_range)[index]),
                "param type copyability"
            );
            assert_eq!(
                arena.modes(pool_range)[index],
                twin_arena.modes(twin_range)[index],
                "param mode"
            );
            assert_eq!(
                arena.comptime(pool_range)[index],
                twin_arena.comptime(twin_range)[index],
                "param comptime"
            );
        }
    }

    fn struct_body(
        fields: Vec<(&str, DType)>,
        is_copy: bool,
        is_linear: bool,
    ) -> DurableNominalBody<Key, Module> {
        DurableNominalBody::Struct {
            fields: fields
                .into_iter()
                .map(|(name, ty)| (Arc::from(name), ty))
                .collect(),
            is_copy,
            is_linear,
        }
    }

    fn enum_body(variants: Vec<(&str, Vec<DType>)>) -> DurableNominalBody<Key, Module> {
        DurableNominalBody::Enum {
            variants: variants
                .into_iter()
                .map(|(name, payload)| (Arc::from(name), payload))
                .collect(),
        }
    }

    fn named(
        name: &str,
        module: &str,
        is_public: bool,
        body: DurableNominalBody<Key, Module>,
    ) -> DurableNominal<Key, Module> {
        DurableNominal {
            name: Arc::from(name),
            module_path: Arc::from(module),
            is_public,
            is_builtin: false,
            lang_item: None,
            is_repr_c: false,
            body,
        }
    }

    /// A local mirror of `Sema::format_type_name` (minus the body-local
    /// `ctor_type_displays`, which is out of 2a) so display parity is asserted
    /// through the same reads the analyzer performs. Recurses through pool
    /// indices, so it is index-independent and safe to compare across two pools.
    fn render(pool: &TypeInternPool, ty: Type) -> String {
        use crate::types::TypeKind;
        match ty.kind() {
            TypeKind::I8 => "i8".into(),
            TypeKind::I16 => "i16".into(),
            TypeKind::I32 => "i32".into(),
            TypeKind::I64 => "i64".into(),
            TypeKind::U8 => "u8".into(),
            TypeKind::U16 => "u16".into(),
            TypeKind::U32 => "u32".into(),
            TypeKind::U64 => "u64".into(),
            TypeKind::Bool => "bool".into(),
            TypeKind::Unit => "()".into(),
            TypeKind::Never => "!".into(),
            TypeKind::Error => "<error>".into(),
            TypeKind::Struct(id) => pool.struct_def(id).name,
            TypeKind::Enum(id) => pool.enum_def(id).name,
            TypeKind::Array(id) => {
                let (element, len) = pool.array_def(id);
                format!("[{}; {}]", render(pool, element), len)
            }
            TypeKind::PtrConst(id) => format!("ptr const {}", render(pool, pool.ptr_const_def(id))),
            TypeKind::PtrMut(id) => format!("ptr mut {}", render(pool, pool.ptr_mut_def(id))),
            TypeKind::Module(_) => "<module>".into(),
            TypeKind::ComptimeType => "type".into(),
        }
    }

    /// A local mirror of `Sema::is_type_copy`, likewise index-independent.
    fn is_copy(pool: &TypeInternPool, ty: Type) -> bool {
        use crate::types::TypeKind;
        match ty.kind() {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Unit
            | TypeKind::Never
            | TypeKind::Error
            | TypeKind::Module(_)
            | TypeKind::ComptimeType
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_) => true,
            TypeKind::Enum(id) => pool
                .enum_def(id)
                .variant_payloads
                .iter()
                .flatten()
                .all(|&ty| is_copy(pool, ty)),
            TypeKind::Struct(id) => pool.struct_def(id).is_copy,
            TypeKind::Array(id) => is_copy(pool, pool.array_def(id).0),
        }
    }

    // ----- Epoch-registration twin -------------------------------------------
    //
    // A twin `TypeInternPool` populated through the exact registration
    // primitives the epoch uses (`sema/declarations.rs`): `declare_struct` /
    // `complete_declared_struct`, `declare_enum` / `complete_declared_enum`,
    // `set_symbol_paths`, `set_struct_lang_item`. The pool under test drives the
    // same primitives from durable metadata; comparing the two proves the pool
    // assembles a byte-equivalent `StructDef` / `EnumDef` and registration.
    //
    // The twin uses the same body-local `FileId` the pool assigns for a single
    // module (`FileId::new(1)`), so `file_id` and the mangled symbol match too.

    const TWIN_FILE: FileId = FileId::new(1);

    fn twin_pool(module_path: &str) -> (TypeInternPool, ThreadedRodeo) {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        pool.set_symbol_paths(HashMap::from([(TWIN_FILE, module_path.to_owned())]));
        (pool, interner)
    }

    fn twin_declare_struct(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        is_copy: bool,
        is_linear: bool,
        is_pub: bool,
        fields: Vec<(&str, Type)>,
        lang_item: Option<LangItem>,
    ) -> StructId {
        let symbol = interner.get_or_intern(name);
        let (id, _) = pool.declare_struct(
            symbol,
            StructDef {
                name: name.to_owned(),
                fields: Vec::new(),
                is_copy,
                is_linear,
                destructor: None,
                is_builtin: false,
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        if let Some(lang_item) = lang_item {
            pool.set_struct_lang_item(id, lang_item);
        }
        pool.complete_declared_struct(
            id,
            StructDef {
                name: name.to_owned(),
                fields: fields
                    .into_iter()
                    .map(|(name, ty)| StructField {
                        name: name.to_owned(),
                        ty,
                    })
                    .collect(),
                is_copy,
                is_linear,
                destructor: None,
                is_builtin: false,
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        id
    }

    fn twin_declare_enum(
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
        name: &str,
        is_pub: bool,
        variants: Vec<(&str, Vec<Type>)>,
    ) -> EnumId {
        let symbol = interner.get_or_intern(name);
        let variant_names: Vec<String> = variants.iter().map(|(n, _)| (*n).to_owned()).collect();
        let (id, _) = pool.declare_enum(
            symbol,
            EnumDef {
                name: name.to_owned(),
                variants: variant_names.clone(),
                variant_payloads: Vec::new(),
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        pool.complete_declared_enum(
            id,
            EnumDef {
                name: name.to_owned(),
                variants: variant_names,
                variant_payloads: variants.into_iter().map(|(_, p)| p).collect(),
                is_pub,
                file_id: TWIN_FILE,
            },
        );
        id
    }

    fn assert_struct_metadata_equal(
        pool: &TypeInternPool,
        pool_id: StructId,
        twin: &TypeInternPool,
        twin_id: StructId,
    ) {
        let a = pool.struct_def(pool_id);
        let b = twin.struct_def(twin_id);
        assert_eq!(a.name, b.name, "struct name");
        assert_eq!(a.is_copy, b.is_copy, "struct is_copy");
        assert_eq!(a.is_linear, b.is_linear, "struct is_linear");
        assert_eq!(a.is_pub, b.is_pub, "struct is_pub");
        assert_eq!(a.is_builtin, b.is_builtin, "struct is_builtin");
        assert_eq!(a.destructor, b.destructor, "struct destructor");
        assert_eq!(a.file_id, b.file_id, "struct file_id");
        assert_eq!(a.fields.len(), b.fields.len(), "struct field count");
        for (fa, fb) in a.fields.iter().zip(b.fields.iter()) {
            assert_eq!(fa.name, fb.name, "field name");
            assert_eq!(
                render(pool, fa.ty),
                render(twin, fb.ty),
                "field type display"
            );
            assert_eq!(
                is_copy(pool, fa.ty),
                is_copy(twin, fb.ty),
                "field type copyability"
            );
        }
        assert_eq!(
            pool.struct_symbol_name(pool_id),
            twin.struct_symbol_name(twin_id),
            "struct symbol name"
        );
        assert_eq!(
            is_copy(pool, Type::new_struct(pool_id)),
            is_copy(twin, Type::new_struct(twin_id))
        );
        assert_eq!(
            render(pool, Type::new_struct(pool_id)),
            render(twin, Type::new_struct(twin_id))
        );
    }

    #[test]
    fn mints_once_and_dedups_named_struct() {
        let mut pool = pool([(
            0,
            named(
                "Point",
                "pkg/geom.rue",
                true,
                struct_body(vec![("x", DType::I64), ("y", DType::I64)], false, false),
            ),
        )]);

        let before = pool.type_pool().len();
        let first = pool.resolve(&DType::Nominal(0)).unwrap();
        let after_first = pool.type_pool().len();
        // Double consult: same id, and the pool grows not at all.
        let second = pool.resolve(&DType::Nominal(0)).unwrap();
        assert_eq!(first, second, "repeat consult returns the same id");
        assert_eq!(
            pool.type_pool().len(),
            after_first,
            "repeat consult mints nothing new"
        );
        assert!(after_first > before, "first consult minted an identity");
        assert_eq!(render(pool.type_pool(), first), "Point");
    }

    #[test]
    fn struct_metadata_matches_epoch_twin() {
        // A copy struct and a non-copy struct, both with primitive fields.
        for (is_copy_flag, is_linear_flag, name) in
            [(true, false, "CopyPair"), (false, false, "MovePair")]
        {
            let mut pool = pool([(
                0,
                named(
                    name,
                    "pkg/data.rue",
                    true,
                    struct_body(
                        vec![("a", DType::I32), ("b", DType::Bool)],
                        is_copy_flag,
                        is_linear_flag,
                    ),
                ),
            )]);
            let ty = pool.resolve(&DType::Nominal(0)).unwrap();

            let (twin, twin_interner) = twin_pool("pkg/data.rue");
            let twin_id = twin_declare_struct(
                &twin,
                &twin_interner,
                name,
                is_copy_flag,
                is_linear_flag,
                true,
                vec![("a", Type::I32), ("b", Type::BOOL)],
                None,
            );

            assert_struct_metadata_equal(pool.type_pool(), ty.as_struct().unwrap(), &twin, twin_id);
        }
    }

    #[test]
    fn enum_metadata_matches_epoch_twin() {
        // Payload-bearing and discriminant-only variants; copyability recurses.
        let mut pool = pool([(
            0,
            named(
                "Shape",
                "pkg/geom.rue",
                true,
                enum_body(vec![
                    ("Dot", vec![]),
                    ("Line", vec![DType::I64, DType::I64]),
                ]),
            ),
        )]);
        let ty = pool.resolve(&DType::Nominal(0)).unwrap();

        let (twin, twin_interner) = twin_pool("pkg/geom.rue");
        let twin_id = twin_declare_enum(
            &twin,
            &twin_interner,
            "Shape",
            true,
            vec![("Dot", vec![]), ("Line", vec![Type::I64, Type::I64])],
        );

        let a = pool.type_pool().enum_def(ty.as_enum().unwrap());
        let b = twin.enum_def(twin_id);
        assert_eq!(a.name, b.name);
        assert_eq!(a.variants, b.variants);
        assert_eq!(a.is_pub, b.is_pub);
        assert_eq!(a.file_id, b.file_id);
        assert_eq!(a.variant_payloads.len(), b.variant_payloads.len());
        for (pa, pb) in a.variant_payloads.iter().zip(b.variant_payloads.iter()) {
            assert_eq!(pa.len(), pb.len());
            for (ta, tb) in pa.iter().zip(pb.iter()) {
                assert_eq!(render(pool.type_pool(), *ta), render(&twin, *tb));
            }
        }
        assert_eq!(
            pool.type_pool().enum_symbol_name(ty.as_enum().unwrap()),
            twin.enum_symbol_name(twin_id)
        );
        assert_eq!(
            is_copy(pool.type_pool(), ty),
            is_copy(&twin, Type::new_enum(twin_id))
        );
        assert_eq!(render(pool.type_pool(), ty), "Shape");
    }

    #[test]
    fn non_copy_field_makes_struct_non_copy_reads_consistent() {
        // Struct with a non-copy nominal field: is_type_copy reads the struct's
        // own @copy flag, and format renders the nested name.
        let mut pool = pool([
            (
                0,
                named(
                    "Owner",
                    "pkg/own.rue",
                    true,
                    struct_body(vec![("h", DType::Nominal(1))], false, false),
                ),
            ),
            (
                1,
                named(
                    "Handle",
                    "pkg/own.rue",
                    true,
                    struct_body(vec![("raw", DType::U64)], false, false),
                ),
            ),
        ]);
        let owner = pool.resolve(&DType::Nominal(0)).unwrap();
        let def = pool.type_pool().struct_def(owner.as_struct().unwrap());
        assert_eq!(def.fields.len(), 1);
        assert_eq!(render(pool.type_pool(), def.fields[0].ty), "Handle");
        assert!(!is_copy(pool.type_pool(), owner));
    }

    #[test]
    fn nested_nominal_dedups_shared_child() {
        let mut pool = pool([
            (
                0,
                named(
                    "Pair",
                    "pkg/p.rue",
                    true,
                    struct_body(
                        vec![("l", DType::Nominal(2)), ("r", DType::Nominal(2))],
                        false,
                        false,
                    ),
                ),
            ),
            (
                2,
                named(
                    "Leaf",
                    "pkg/p.rue",
                    true,
                    struct_body(vec![("v", DType::I32)], true, false),
                ),
            ),
        ]);
        pool.resolve(&DType::Nominal(0)).unwrap();
        // The shared child was minted once; a direct consult returns that id.
        let leaf_first = pool.resolve(&DType::Nominal(2)).unwrap();
        let len_after = pool.type_pool().len();
        let leaf_second = pool.resolve(&DType::Nominal(2)).unwrap();
        assert_eq!(leaf_first, leaf_second);
        assert_eq!(
            pool.type_pool().len(),
            len_after,
            "no re-mint of shared child"
        );
    }

    #[test]
    fn recursive_struct_through_pointer_mints_once() {
        // Node { next: ptr mut Node } — the recursive reference resolves to the
        // shell id registered before field resolution.
        let mut pool = pool([(
            0,
            named(
                "Node",
                "pkg/list.rue",
                true,
                struct_body(
                    vec![
                        ("value", DType::I64),
                        ("next", DType::PtrMut(Box::new(DType::Nominal(0)))),
                    ],
                    false,
                    false,
                ),
            ),
        )]);
        let node = pool.resolve(&DType::Nominal(0)).unwrap();
        let node_id = node.as_struct().unwrap();
        let def = pool.type_pool().struct_def(node_id);
        assert_eq!(def.fields.len(), 2);
        assert_eq!(render(pool.type_pool(), def.fields[1].ty), "ptr mut Node");
        // Re-consulting the recursive nominal yields the same id (dedup).
        assert_eq!(pool.resolve(&DType::Nominal(0)).unwrap(), node);
    }

    #[test]
    fn structural_wraps_intern_and_dedup() {
        let mut pool = pool([(
            0,
            named(
                "Cell",
                "pkg/c.rue",
                true,
                struct_body(vec![("v", DType::I32)], true, false),
            ),
        )]);

        // Array dedup.
        let array_ty = DType::Array {
            element: Box::new(DType::I32),
            len: 4,
        };
        let a1 = pool.resolve(&array_ty).unwrap();
        let len_after = pool.type_pool().len();
        let a2 = pool.resolve(&array_ty).unwrap();
        assert_eq!(a1, a2);
        assert_eq!(pool.type_pool().len(), len_after, "array interning dedups");
        assert_eq!(render(pool.type_pool(), a1), "[i32; 4]");

        // Pointer dedup.
        let ptr_ty = DType::PtrConst(Box::new(DType::I32));
        let p1 = pool.resolve(&ptr_ty).unwrap();
        let len_after_ptr = pool.type_pool().len();
        let p2 = pool.resolve(&ptr_ty).unwrap();
        assert_eq!(p1, p2);
        assert_eq!(
            pool.type_pool().len(),
            len_after_ptr,
            "ptr interning dedups"
        );
        assert_eq!(render(pool.type_pool(), p1), "ptr const i32");

        // Array of a nominal renders through the minted child.
        let array_of_cell = DType::Array {
            element: Box::new(DType::Nominal(0)),
            len: 3,
        };
        let ac = pool.resolve(&array_of_cell).unwrap();
        assert_eq!(render(pool.type_pool(), ac), "[Cell; 3]");
        assert!(is_copy(pool.type_pool(), ac), "array of @copy Cell is copy");
    }

    #[test]
    fn display_parity_nominal_array_ptr_against_twin() {
        let mut pool = pool([(
            0,
            named(
                "Widget",
                "pkg/ui.rue",
                true,
                struct_body(vec![("id", DType::U32)], false, false),
            ),
        )]);
        let (twin, twin_interner) = twin_pool("pkg/ui.rue");
        let twin_id = twin_declare_struct(
            &twin,
            &twin_interner,
            "Widget",
            false,
            false,
            true,
            vec![("id", Type::U32)],
            None,
        );

        // Nominal.
        let pool_widget = pool.resolve(&DType::Nominal(0)).unwrap();
        let twin_widget = Type::new_struct(twin_id);
        assert_eq!(
            render(pool.type_pool(), pool_widget),
            render(&twin, twin_widget)
        );

        // Array of nominal.
        let pool_arr = pool
            .resolve(&DType::Array {
                element: Box::new(DType::Nominal(0)),
                len: 3,
            })
            .unwrap();
        let twin_arr = twin.try_intern_array(twin_widget, 3).unwrap();
        assert_eq!(render(pool.type_pool(), pool_arr), render(&twin, twin_arr));
        assert_eq!(render(pool.type_pool(), pool_arr), "[Widget; 3]");

        // Pointer of nominal.
        let pool_ptr = pool
            .resolve(&DType::PtrConst(Box::new(DType::Nominal(0))))
            .unwrap();
        let twin_ptr = twin.try_intern_ptr_const(twin_widget).unwrap();
        assert_eq!(render(pool.type_pool(), pool_ptr), render(&twin, twin_ptr));

        // Nested ptr-of-ptr.
        let pool_pp = pool
            .resolve(&DType::PtrMut(Box::new(DType::PtrConst(Box::new(
                DType::I32,
            )))))
            .unwrap();
        assert_eq!(render(pool.type_pool(), pool_pp), "ptr mut ptr const i32");
    }

    #[test]
    fn qualified_symbol_matches_twin_and_lang_item_is_exempt() {
        // A user nominal is unconditionally file-qualified; a lang-item nominal
        // keeps its bare name.
        let mut pool = pool([
            (
                0,
                named(
                    "Buffer",
                    "pkg/buf.rue",
                    true,
                    struct_body(vec![("len", DType::U64)], false, false),
                ),
            ),
            (
                1,
                DurableNominal {
                    name: Arc::from("StrBuf"),
                    module_path: Arc::from("\0rue-std/strbuf.rue"),
                    is_public: true,
                    is_builtin: false,
                    lang_item: Some(LangItem::StrBuf),
                    is_repr_c: false,
                    body: struct_body(vec![("len", DType::U64)], false, false),
                },
            ),
        ]);

        let buffer = pool.resolve(&DType::Nominal(0)).unwrap();
        let buffer_id = buffer.as_struct().unwrap();
        let symbol = pool.type_pool().struct_symbol_name(buffer_id);
        assert!(
            symbol.starts_with("Buffer$"),
            "user nominal is file-qualified, got {symbol}"
        );

        let (twin, twin_interner) = twin_pool("pkg/buf.rue");
        let twin_id = twin_declare_struct(
            &twin,
            &twin_interner,
            "Buffer",
            false,
            false,
            true,
            vec![("len", Type::U64)],
            None,
        );
        assert_eq!(symbol, twin.struct_symbol_name(twin_id));

        // Lang-item nominal keeps its bare name.
        let strbuf = pool.resolve(&DType::Nominal(1)).unwrap();
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(strbuf.as_struct().unwrap()),
            "StrBuf"
        );
    }

    #[test]
    fn builtin_nominal_and_str_resolve_to_preregistered() {
        let mut pool = pool([]);

        // Builtin enum (Arch) resolves to the pre-registered enum.
        let arch = pool
            .resolve(&DType::BuiltinNominal {
                name: Arc::from("Arch"),
                kind: SemanticImportNominalKind::Enum,
            })
            .unwrap();
        assert_eq!(render(pool.type_pool(), arch), "Arch");
        assert_eq!(
            pool.type_pool().enum_symbol_name(arch.as_enum().unwrap()),
            "Arch"
        );

        // The core `str` identity.
        let str_ty = pool
            .resolve(&DType::BuiltinNominal {
                name: Arc::from("str"),
                kind: SemanticImportNominalKind::Struct,
            })
            .unwrap();
        assert_eq!(render(pool.type_pool(), str_ty), "str");
        assert!(is_copy(pool.type_pool(), str_ty));
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(str_ty.as_struct().unwrap()),
            "str",
            "builtin keeps its bare name"
        );

        // Wrong kind and unknown builtin fail closed.
        assert_eq!(
            pool.resolve(&DType::BuiltinNominal {
                name: Arc::from("Arch"),
                kind: SemanticImportNominalKind::Struct,
            }),
            Err(IdentityMintError::BuiltinNominalKindMismatch)
        );
        assert_eq!(
            pool.resolve(&DType::BuiltinNominal {
                name: Arc::from("Nope"),
                kind: SemanticImportNominalKind::Enum,
            }),
            Err(IdentityMintError::UnknownBuiltinNominal)
        );
    }

    #[test]
    fn anonymous_arm_resolves_by_lookup() {
        let mut pool = pool([(
            0,
            named(
                "Cell",
                "pkg/c.rue",
                true,
                struct_body(vec![("v", DType::I32)], true, false),
            ),
        )]);
        let cell = pool.resolve(&DType::Nominal(0)).unwrap();

        let anon_key = AnonymousNominalKey {
            kind: AnonymousNominalKind::Struct,
            producer: StableProducerId::Definition(0u32),
            anchor: rue_rir::RirStructuralAnchor::new(
                Vec::<rue_rir::RirStructuralPathSegment>::new(),
            ),
            arguments: CanonicalArguments::default(),
        };

        // Before registration, the anonymous arm fails closed.
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(anon_key.clone())),
            Err(IdentityMintError::MissingAnonymous)
        );

        // After the issuing machinery records the id, it resolves by lookup.
        pool.register_issued_anonymous(anon_key.clone(), cell);
        assert_eq!(
            pool.resolve(&DType::AnonymousNominal(anon_key)).unwrap(),
            cell
        );
    }

    #[test]
    fn missing_and_deferred_arms_fail_closed() {
        let mut pool = pool([]);
        assert_eq!(
            pool.resolve(&DType::Nominal(7)),
            Err(IdentityMintError::MissingNominal)
        );
        assert_eq!(
            pool.resolve(&DType::Module(Arc::from("pkg/m.rue"))),
            Err(IdentityMintError::Deferred("module identity"))
        );
        assert_eq!(
            pool.resolve(&DType::GenericParameter(0)),
            Err(IdentityMintError::Deferred("generic parameter"))
        );
    }

    #[test]
    fn slice_arm_mints_generated_struct_and_dedups() {
        // The Slice arm is the one resolve arm that mints a fresh struct
        // (mirroring `import_type_local`); pin its registration byte-for-byte.
        let mut pool = pool([]);
        let slice = pool
            .resolve(&DType::Slice {
                name: Arc::from("__slice_i64"),
                element: Box::new(DType::I64),
            })
            .unwrap();
        let id = slice.as_struct().unwrap();
        let def = pool.type_pool().struct_def(id);
        assert_eq!(def.name, "__slice_i64");
        assert!(def.is_copy, "slice headers are copy");
        assert!(def.is_builtin, "slice structs register as builtin");
        assert_eq!(def.fields.len(), 2, "ptr + len: {:?}", def.fields);
        assert_eq!(def.fields[0].name, "ptr");
        assert_eq!(def.fields[1].name, "len");
        let again = pool
            .resolve(&DType::Slice {
                name: Arc::from("__slice_i64"),
                element: Box::new(DType::I64),
            })
            .unwrap();
        assert_eq!(slice, again, "repeat slice consult dedups");
    }

    #[test]
    fn nested_field_nominal_symbol_is_module_qualified_per_field() {
        // Two same-named `Handle` nominals in DIFFERENT modules must mint
        // distinct ids whose qualified symbols carry their own module
        // components — the render/is_copy mirrors cannot see this, so assert
        // through the production `struct_symbol_name` per FIELD nominal.
        let mut pool = pool([
            (
                0,
                named(
                    "Owner",
                    "pkg/owner.rue",
                    true,
                    struct_body(
                        vec![("a", DType::Nominal(1)), ("b", DType::Nominal(2))],
                        false,
                        false,
                    ),
                ),
            ),
            (
                1,
                named(
                    "Handle",
                    "pkg/alpha.rue",
                    true,
                    struct_body(vec![], true, false),
                ),
            ),
            (
                2,
                named(
                    "Handle",
                    "pkg/beta.rue",
                    true,
                    struct_body(vec![], true, false),
                ),
            ),
        ]);
        let owner = pool.resolve(&DType::Nominal(0)).unwrap();
        let owner_def = pool.type_pool().struct_def(owner.as_struct().unwrap());
        let a_id = owner_def.fields[0].ty.as_struct().unwrap();
        let b_id = owner_def.fields[1].ty.as_struct().unwrap();
        assert_ne!(
            a_id, b_id,
            "same-named nominals from distinct modules stay distinct"
        );
        let a_symbol = pool.type_pool().struct_symbol_name(a_id);
        let b_symbol = pool.type_pool().struct_symbol_name(b_id);
        assert_ne!(
            a_symbol, b_symbol,
            "field nominal symbols carry their own module component"
        );
        assert!(
            a_symbol.contains('$') && b_symbol.contains('$'),
            "{a_symbol} / {b_symbol}"
        );
    }

    #[test]
    fn repr_c_registers_like_the_epoch_shell_phase() {
        let mut repr = named(
            "Raw",
            "pkg/ffi.rue",
            true,
            struct_body(vec![("x", DType::I64)], true, false),
        );
        repr.is_repr_c = true;
        let mut pool = pool([
            (0, repr),
            (
                1,
                named(
                    "Plain",
                    "pkg/ffi.rue",
                    true,
                    struct_body(vec![("x", DType::I64)], true, false),
                ),
            ),
        ]);
        let raw = pool
            .resolve(&DType::Nominal(0))
            .unwrap()
            .as_struct()
            .unwrap();
        let plain = pool
            .resolve(&DType::Nominal(1))
            .unwrap()
            .as_struct()
            .unwrap();
        assert!(pool.type_pool().is_struct_repr_c(raw));
        assert!(!pool.type_pool().is_struct_repr_c(plain));
    }

    #[test]
    fn failed_mint_poisons_the_key_and_repeat_consult_reerrors() {
        // A field that cannot resolve (generic parameter is a deferred arm)
        // fails the mint AFTER the shell pre-registered. The repeat consult
        // must re-error — never hand out the incomplete shell, whose
        // `struct_def` read would panic.
        let mut pool = pool([(
            0,
            named(
                "Broken",
                "pkg/broken.rue",
                true,
                struct_body(vec![("bad", DType::GenericParameter(0))], false, false),
            ),
        )]);
        let first = pool.resolve(&DType::Nominal(0));
        assert_eq!(first, Err(IdentityMintError::Deferred("generic parameter")));
        let second = pool.resolve(&DType::Nominal(0));
        assert_eq!(
            second,
            Err(IdentityMintError::Deferred("generic parameter")),
            "poisoned key re-errors"
        );
    }

    // ----- Callable identity family (r4a-2b) ---------------------------------

    #[test]
    fn function_signature_matches_epoch_twin() {
        // A three-parameter function (by-value, borrow, comptime-value) with an
        // i64 return, built through the pool and through the epoch primitives.
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("i64"));

        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![
                        param("a", DType::I32, SemanticParameterMode::Value, false),
                        param("b", DType::Bool, SemanticParameterMode::Borrow, false),
                        param("n", DType::U64, SemanticParameterMode::Value, true),
                    ],
                    DType::I64,
                    true,
                    false,
                ),
            )],
            [],
        );
        let info = pool.resolve_function(&0, handle).unwrap();

        // Epoch twin: the same params allocated through `ParamArena::alloc`.
        let twin_pool = TypeInternPool::new();
        let twin_interner = ThreadedRodeo::new();
        let mut twin_arena = ParamArena::new();
        let twin_range = twin_arena.alloc(
            [
                twin_interner.get_or_intern("a"),
                twin_interner.get_or_intern("b"),
                twin_interner.get_or_intern("n"),
            ],
            [Type::I32, Type::BOOL, Type::U64],
            [
                RirParamMode::Normal,
                RirParamMode::Borrow,
                RirParamMode::Normal,
            ],
            [false, false, true],
        );

        // Durable-derived fields.
        assert_param_range_equal(
            &pool,
            info.params,
            &twin_arena,
            &twin_interner,
            twin_range,
            &twin_pool,
        );
        assert_eq!(
            render(pool.type_pool(), info.return_type),
            render(&twin_pool, Type::I64)
        );
        assert!(
            info.is_generic,
            "a comptime-value param marks the fn generic"
        );
        assert!(info.is_pub);
        assert!(!info.is_unchecked);

        // Request/RIR passthrough: exactly the handle, nothing fabricated.
        assert_eq!(info.body, handle.body);
        assert_eq!(info.declaration, handle.declaration);
        assert_eq!(info.span, handle.span);
        assert_eq!(info.return_type_sym, handle.return_type_sym);
        assert_eq!(info.is_extern, handle.is_extern);
        assert_eq!(info.is_c_export, handle.is_c_export);
        assert_eq!(info.allow_unused_function, handle.allow_unused_function);
        assert_eq!(info.allow_unused_variable, handle.allow_unused_variable);
        assert_eq!(info.allow_unreachable_code, handle.allow_unreachable_code);
        assert_eq!(info.file_id, handle.file_id);
    }

    #[test]
    fn function_signature_mints_params_once_and_dedups() {
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![param("x", DType::I32, SemanticParameterMode::Value, false)],
                    DType::Unit,
                    false,
                    false,
                ),
            )],
            [],
        );

        let before = pool.param_arena().total_params();
        let first = pool.resolve_function(&0, handle).unwrap();
        let after_first = pool.param_arena().total_params();
        let second = pool.resolve_function(&0, handle).unwrap();
        assert_eq!(
            first.params, second.params,
            "repeat consult returns the same ParamRange"
        );
        assert_eq!(
            pool.param_arena().total_params(),
            after_first,
            "repeat consult interns no new params"
        );
        assert!(after_first > before, "first consult interned params");
    }

    #[test]
    fn method_signature_matches_epoch_twin() {
        // `fn (self, delta: i32) -> bool` on `Widget`.
        let mut pool = callable_pool(
            [(
                1,
                named(
                    "Widget",
                    "pkg/ui.rue",
                    true,
                    struct_body(vec![("id", DType::U32)], false, false),
                ),
            )],
            [],
            [(
                0,
                durable_method(
                    DType::Nominal(1),
                    vec![param(
                        "delta",
                        DType::I32,
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::Bool,
                    true,
                ),
            )],
        );
        let handle = method_handle();
        let info = pool.resolve_method(&0, handle).unwrap();

        // Twin: the same nominal + params through the epoch primitives.
        let (twin, twin_interner) = twin_pool("pkg/ui.rue");
        let twin_id = twin_declare_struct(
            &twin,
            &twin_interner,
            "Widget",
            false,
            false,
            true,
            vec![("id", Type::U32)],
            None,
        );
        let mut twin_arena = ParamArena::new();
        let twin_range = twin_arena.alloc(
            [twin_interner.get_or_intern("delta")],
            [Type::I32],
            [RirParamMode::Normal],
            [false],
        );

        // Durable-derived: receiver, has_self, return type, params.
        assert_eq!(
            render(pool.type_pool(), info.struct_type),
            render(&twin, Type::new_struct(twin_id))
        );
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(info.struct_type.as_struct().unwrap()),
            twin.struct_symbol_name(twin_id),
            "receiver symbol name parity"
        );
        assert!(info.has_self);
        assert_eq!(
            render(pool.type_pool(), info.return_type),
            render(&twin, Type::BOOL)
        );
        assert_param_range_equal(
            &pool,
            info.params,
            &twin_arena,
            &twin_interner,
            twin_range,
            &twin,
        );

        // Request/RIR passthrough.
        assert_eq!(info.body, handle.body);
        assert_eq!(info.span, handle.span);
        assert_eq!(info.self_mode, handle.self_mode);
        assert_eq!(info.self_is_mut, handle.self_is_mut);
    }

    #[test]
    fn parameter_modes_map_to_rir_modes() {
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![
                        param("v", DType::I32, SemanticParameterMode::Value, false),
                        param("b", DType::I32, SemanticParameterMode::Borrow, false),
                        param("i", DType::I32, SemanticParameterMode::Inout, false),
                    ],
                    DType::Unit,
                    false,
                    false,
                ),
            )],
            [],
        );
        let info = pool.resolve_function(&0, handle).unwrap();
        assert_eq!(
            pool.param_arena().modes(info.params),
            &[
                RirParamMode::Normal,
                RirParamMode::Borrow,
                RirParamMode::Inout
            ]
        );
        assert!(!info.is_generic, "no comptime param means non-generic");
    }

    #[test]
    fn parameter_nominal_type_resolves_and_dedups_through_pool() {
        // A parameter typed by a named nominal resolves through the same 2a
        // machinery and reuses an already-minted id (compose, don't duplicate).
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [(
                5,
                named(
                    "Cell",
                    "pkg/c.rue",
                    true,
                    struct_body(vec![("v", DType::I32)], true, false),
                ),
            )],
            [(
                0,
                durable_function(
                    vec![param(
                        "cell",
                        DType::Nominal(5),
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::Unit,
                    true,
                    false,
                ),
            )],
            [],
        );
        // Mint the nominal directly, then assert the param reuses its id.
        let direct = pool.resolve(&DType::Nominal(5)).unwrap();
        let len_after_nominal = pool.type_pool().len();
        let info = pool.resolve_function(&0, handle).unwrap();
        let param_ty = pool.param_arena().types(info.params)[0];
        assert_eq!(param_ty, direct, "param nominal reuses the minted id");
        assert_eq!(
            pool.type_pool().len(),
            len_after_nominal,
            "param resolution mints no new nominal"
        );
        assert_eq!(render(pool.type_pool(), param_ty), "Cell");
    }

    #[test]
    fn generic_parameter_type_refuses_and_poisons_callable() {
        // A parameter typed by a generic parameter is a deferred 2a arm: the
        // callable refuses (never approximates) and the key is poisoned.
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![param(
                        "x",
                        DType::GenericParameter(0),
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::Unit,
                    false,
                    false,
                ),
            )],
            [],
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter")
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter"),
            "poisoned callable re-errors"
        );
    }

    #[test]
    fn return_type_refusal_poisons_after_params_interned() {
        // Params resolve (and intern into the arena), but the return type is a
        // deferred arm. The key poisons, and the repeat consult short-circuits
        // without re-interning the orphaned params — poison-on-partial-failure.
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("t"));
        let mut pool = callable_pool(
            [],
            [(
                0,
                durable_function(
                    vec![param("x", DType::I32, SemanticParameterMode::Value, false)],
                    DType::GenericParameter(0),
                    false,
                    false,
                ),
            )],
            [],
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter")
        );
        let total_after_first = pool.param_arena().total_params();
        assert_eq!(
            total_after_first, 1,
            "the param interned before the return-type failure"
        );
        assert_eq!(
            pool.resolve_function(&0, handle).unwrap_err(),
            IdentityMintError::Deferred("generic parameter"),
            "poisoned callable re-errors"
        );
        assert_eq!(
            pool.param_arena().total_params(),
            total_after_first,
            "poison short-circuits: no re-interning of orphaned params"
        );
    }

    #[test]
    fn missing_callable_key_fails_closed() {
        let aux = ThreadedRodeo::new();
        let handle = fn_handle(aux.get_or_intern("unit"));
        let mut pool = callable_pool([], [], []);
        assert_eq!(
            pool.resolve_function(&9, handle).unwrap_err(),
            IdentityMintError::MissingCallable
        );
        assert_eq!(
            pool.resolve_method(&9, method_handle()).unwrap_err(),
            IdentityMintError::MissingCallable
        );
    }

    // ----- RIR-index answer surface (r4a-2c) ---------------------------------
    //
    // Twin-parity for the three endpoint seam ops. Each test builds a real
    // program's `Rir` through the production lex/parse/astgen path, binds its
    // declarations through the epoch, and compares the pool-side `BodyRirIndex`
    // answers against the epoch's `EpochFacts` over the SAME `Rir` — then the
    // capstone assembles a complete `FunctionInfo` / `MethodInfo` from the index
    // (2c) + a durable signature (2b, resolving types through 2a) and proves it
    // equals production's populated info struct field-for-field.

    use crate::sema::body_endpoint::{BodyEndpointProvider, endpoint_facts};
    use crate::sema::{BodySema, BoundSema, Sema};

    /// Lower one source file to `Rir` through the production frontend.
    fn lower_rir(source: &str, file_id: FileId) -> (Rir, ThreadedRodeo) {
        use rue_lexer::Lexer;
        use rue_parser::Parser;
        use rue_rir::AstGen;
        let interner = ThreadedRodeo::default();
        let lexer = Lexer::with_interner_and_file_id(source, interner, file_id);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        (rir, interner)
    }

    /// Bind declarations through the epoch so `functions`, `methods`,
    /// `named_method_declarations`, and the `type_pool` are populated — the
    /// state the `EpochFacts` twin reads.
    fn bind<'a>(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        module_path: &str,
        file_id: FileId,
    ) -> BoundSema<'a> {
        use rue_error::PreviewFeatures;
        let mut sema = Sema::new_synthetic(rir, interner, PreviewFeatures::new());
        sema.set_symbol_paths(HashMap::from([(file_id, module_path.to_owned())]));
        sema.bind_declarations_for_test().unwrap()
    }

    /// Fill a [`FunctionIdentityHandle`] from a free-function declaration the
    /// RIR index located: exactly the request/RIR reads production performs
    /// (`binding_manifest.rs`) — `body` and the `@allow` flags off the RIR, the
    /// pre-resolution return symbol, the RIR-only `is_extern`/`is_c_export`.
    fn fn_handle_from_rir(sema: &BodySema<'_>, declaration: InstRef) -> FunctionIdentityHandle {
        let inst = sema.rir.get(declaration);
        let InstData::FnDecl {
            body,
            return_type,
            is_extern,
            is_c_export,
            directives,
            ..
        } = &inst.data
        else {
            panic!("free-function declaration must be a FnDecl");
        };
        let dirs = sema.rir.directives(directives);
        FunctionIdentityHandle {
            body: *body,
            declaration,
            span: inst.span,
            return_type_sym: *return_type,
            is_extern: *is_extern,
            is_c_export: *is_c_export,
            allow_unused_function: sema.has_allow_directive(dirs.iter(), "unused_function"),
            allow_unused_variable: sema.has_allow_directive(dirs.iter(), "unused_variable"),
            allow_unreachable_code: sema.has_allow_directive(dirs.iter(), "unreachable_code"),
            file_id: inst.span.file_id,
        }
    }

    /// Fill a [`MethodIdentityHandle`] from a method declaration the RIR index
    /// located: `self_mode` / `self_is_mut` are RIR `FnDecl` facts, `body` and
    /// `span` request-carried.
    fn method_handle_from_rir(sema: &BodySema<'_>, declaration: InstRef) -> MethodIdentityHandle {
        let inst = sema.rir.get(declaration);
        let InstData::FnDecl {
            body,
            self_mode,
            self_is_mut,
            ..
        } = &inst.data
        else {
            panic!("method declaration must be a FnDecl");
        };
        MethodIdentityHandle {
            body: *body,
            span: inst.span,
            self_mode: *self_mode,
            self_is_mut: *self_is_mut,
        }
    }

    #[test]
    fn body_rir_index_first_free_function_matches_epoch() {
        let file = FileId::new(7);
        let source = r#"
            struct Named {
                value: i32,
                fn collide() -> i32 { 10 }
            }
            fn collide() -> i32 { 40 }
            fn helper() -> i32 { 1 }
            fn main() -> i32 { collide() }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);
        let bound = bind(&rir, &interner, "pkg/main.rue", file);
        let facts = endpoint_facts(bound.body_sema());

        for name in ["collide", "helper", "main", "nonexistent"] {
            let sym = interner.get(name);
            let pool_ans = sym.and_then(|s| index.first_free_function(s, file));
            let epoch_ans = sym.and_then(|s| facts.first_free_function(s, file));
            assert_eq!(pool_ans, epoch_ans, "first_free_function parity for {name}");
        }

        // The free `collide` resolves; the same-named associated function is not
        // a free candidate (it is a named method).
        let collide = interner.get("collide").unwrap();
        assert!(index.first_free_function(collide, file).is_some());
        // A wrong file fails closed on both sides.
        assert_eq!(index.first_free_function(collide, FileId::new(99)), None);
        assert_eq!(facts.first_free_function(collide, FileId::new(99)), None);
    }

    #[test]
    fn body_rir_index_destructor_matches_epoch() {
        let file = FileId::new(5);
        let source = r#"
            struct Foo { x: i32 }
            drop fn Foo(self) {}
            struct Bar { y: i32 }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);
        let bound = bind(&rir, &interner, "pkg/main.rue", file);
        let facts = endpoint_facts(bound.body_sema());

        let foo = interner.get("Foo").unwrap();
        let pool_foo = index.destructor(file.index(), foo);
        let epoch_foo = facts.destructor(file.index(), foo);
        assert_eq!(pool_foo, epoch_foo, "destructor record parity for Foo");
        assert!(pool_foo.is_some(), "Foo has a destructor");

        // Bar has no destructor; both sides fail closed.
        let bar = interner.get("Bar").unwrap();
        assert_eq!(index.destructor(file.index(), bar), None);
        assert_eq!(facts.destructor(file.index(), bar), None);
    }

    #[test]
    fn body_rir_index_named_method_declaration_matches_epoch() {
        let file = FileId::new(3);
        let source = r#"
            struct Widget {
                id: u32,
                fn bump(self, delta: i32) -> u32 { self.id }
                fn reset() -> u32 { 0 }
            }
            struct Gadget {
                n: i32,
                fn bump(self) -> i32 { self.n }
            }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);
        let bound = bind(&rir, &interner, "pkg/main.rue", file);
        let facts = endpoint_facts(bound.body_sema());

        for (type_name, method) in [
            ("Widget", "bump"),
            ("Widget", "reset"),
            ("Gadget", "bump"),
            ("Widget", "nonexistent"),
        ] {
            let owner = interner.get(type_name).unwrap();
            let method_sym = interner.get(method);
            // The epoch keys by the pool-minted StructId; the index keys by the
            // durable-available (file, type_name) preimage of that StructId.
            let struct_id = facts.struct_by_file_name(file, owner).unwrap();
            let epoch_ans = method_sym.and_then(|m| facts.named_method_declaration(struct_id, m));
            let pool_ans = method_sym.and_then(|m| index.named_method_declaration(file, owner, m));
            assert_eq!(
                pool_ans, epoch_ans,
                "named_method parity for {type_name}.{method}"
            );
        }

        // Widget.bump and Gadget.bump share a method name but are distinct
        // declarations — the owner file+name disambiguates without a StructId.
        let widget = interner.get("Widget").unwrap();
        let gadget = interner.get("Gadget").unwrap();
        let bump = interner.get("bump").unwrap();
        let widget_bump = index.named_method_declaration(file, widget, bump);
        let gadget_bump = index.named_method_declaration(file, gadget, bump);
        assert!(widget_bump.is_some() && gadget_bump.is_some());
        assert_ne!(widget_bump, gadget_bump, "same-named methods stay distinct");
    }

    #[test]
    fn provider_assembles_function_info_composing_2a_2b_2c() {
        // The pool-arc capstone: a provider assembles a complete `FunctionInfo`
        // from the RIR index (2c handle) + the durable signature (2b, whose
        // `Point` parameter resolves through 2a), byte-equal to production.
        let file = FileId::new(7);
        let source = r#"
            pub struct Point { x: i64, y: i64 }
            @allow(unused_function)
            pub fn make(p: Point, n: i32) -> i64 { 0 }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);
        let bound = bind(&rir, &interner, "pkg/main.rue", file);
        let bs = bound.body_sema();
        let facts = endpoint_facts(bs);

        // Production's populated FunctionInfo for `make`.
        let make_src = interner.get("make").unwrap();
        let internal = facts.function_by_file_name(file, make_src).unwrap();
        let prod = facts.function_info(internal).unwrap();

        // 2c: the RIR index locates the declaration; fill the request/RIR handle.
        let declaration = index.first_free_function(make_src, file).unwrap();
        assert_eq!(
            declaration, prod.declaration,
            "index locates the epoch's declaration"
        );
        let handle = fn_handle_from_rir(bs, declaration);

        // 2b + 2a: mint the durable-signature subset from a durable source.
        let mut pool = callable_pool(
            [(
                1,
                named(
                    "Point",
                    "pkg/main.rue",
                    true,
                    struct_body(vec![("x", DType::I64), ("y", DType::I64)], false, false),
                ),
            )],
            [(
                0,
                durable_function(
                    vec![
                        param("p", DType::Nominal(1), SemanticParameterMode::Value, false),
                        param("n", DType::I32, SemanticParameterMode::Value, false),
                    ],
                    DType::I64,
                    true,
                    false,
                ),
            )],
            [],
        );
        let info = pool.resolve_function(&0, handle).unwrap();

        // 2c fields: verbatim RIR passthrough == production, field-for-field.
        assert_eq!(info.body, prod.body);
        assert_eq!(info.declaration, prod.declaration);
        assert_eq!(info.span, prod.span);
        assert_eq!(info.return_type_sym, prod.return_type_sym);
        assert_eq!(info.is_extern, prod.is_extern);
        assert_eq!(info.is_c_export, prod.is_c_export);
        assert_eq!(info.allow_unused_function, prod.allow_unused_function);
        assert!(
            info.allow_unused_function,
            "the @allow(unused_function) flag flowed through the handle"
        );
        assert_eq!(info.allow_unused_variable, prod.allow_unused_variable);
        assert_eq!(info.allow_unreachable_code, prod.allow_unreachable_code);
        assert_eq!(info.file_id, prod.file_id);

        // 2b / 2a fields: durable-derived, compared index-independently.
        assert_eq!(info.is_generic, prod.is_generic);
        assert_eq!(info.is_pub, prod.is_pub);
        assert_eq!(info.is_unchecked, prod.is_unchecked);
        assert_eq!(
            render(pool.type_pool(), info.return_type),
            render(&bs.type_pool, prod.return_type),
            "return type display parity"
        );
        assert_param_range_equal(
            &pool,
            info.params,
            &bs.param_arena,
            bs.interner,
            prod.params,
            &bs.type_pool,
        );
        // The `Point` parameter resolved through 2a to a nominal.
        assert_eq!(
            render(pool.type_pool(), pool.param_arena().types(info.params)[0]),
            "Point"
        );
    }

    #[test]
    fn provider_assembles_method_info_composing_2a_2b_2c() {
        // The method twin of the capstone: assemble a complete `MethodInfo` from
        // the RIR index (2c) + durable method signature (2b, receiver `Widget`
        // through 2a) and prove it equals production's method info.
        let file = FileId::new(3);
        let source = r#"
            pub struct Widget {
                id: u32,
                fn bump(self, delta: i32) -> u32 { self.id }
            }
        "#;
        let (rir, interner) = lower_rir(source, file);
        let index = BodyRirIndex::new(&rir);
        let bound = bind(&rir, &interner, "pkg/main.rue", file);
        let bs = bound.body_sema();
        let facts = endpoint_facts(bs);

        let widget = interner.get("Widget").unwrap();
        let bump = interner.get("bump").unwrap();
        let struct_id = facts.struct_by_file_name(file, widget).unwrap();
        let prod = facts.method_info(struct_id, bump).unwrap();

        // 2c: the RIR index locates the method declaration; fill the handle.
        let declaration = index.named_method_declaration(file, widget, bump).unwrap();
        assert_eq!(
            Some(declaration),
            facts.named_method_declaration(struct_id, bump),
            "index locates the epoch's method declaration"
        );
        let handle = method_handle_from_rir(bs, declaration);

        // 2b + 2a: mint the durable method subset (receiver resolves through 2a).
        let mut pool = callable_pool(
            [(
                1,
                named(
                    "Widget",
                    "pkg/main.rue",
                    true,
                    struct_body(vec![("id", DType::U32)], false, false),
                ),
            )],
            [],
            [(
                0,
                durable_method(
                    DType::Nominal(1),
                    vec![param(
                        "delta",
                        DType::I32,
                        SemanticParameterMode::Value,
                        false,
                    )],
                    DType::U32,
                    true,
                ),
            )],
        );
        let info = pool.resolve_method(&0, handle).unwrap();

        // 2c passthrough.
        assert_eq!(info.body, prod.body);
        assert_eq!(info.span, prod.span);
        assert_eq!(info.self_mode, prod.self_mode);
        assert_eq!(info.self_is_mut, prod.self_is_mut);

        // 2b / 2a durable-derived: receiver, has_self, return type, params.
        assert_eq!(info.has_self, prod.has_self);
        assert!(info.has_self);
        assert_eq!(
            render(pool.type_pool(), info.struct_type),
            render(&bs.type_pool, prod.struct_type),
            "receiver display parity"
        );
        assert_eq!(
            pool.type_pool()
                .struct_symbol_name(info.struct_type.as_struct().unwrap()),
            bs.type_pool
                .struct_symbol_name(prod.struct_type.as_struct().unwrap()),
            "receiver symbol name parity"
        );
        assert_eq!(
            render(pool.type_pool(), info.return_type),
            render(&bs.type_pool, prod.return_type),
            "return type display parity"
        );
        assert_param_range_equal(
            &pool,
            info.params,
            &bs.param_arena,
            bs.interner,
            prod.params,
            &bs.type_pool,
        );
    }
}
