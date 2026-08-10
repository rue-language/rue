//! Declaration gathering for semantic analysis.
//!
//! This module handles the first phase of semantic analysis: gathering all
//! type and function declarations from the RIR. This includes:
//!
//! - Registering struct and enum type names
//! - Resolving struct field types
//! - Collecting function signatures
//! - Collecting method signatures from impl blocks
//! - Validating @copy structs

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_error::{CopyStructNonCopyFieldError, PreviewFeature, ice};
use rue_rir::InstData;
use rue_rir::{InstRef, RirParamMode};
use rue_span::{FileId, Span};

use super::{ConstInfo, ConstValue, DeclarationPhase, InferenceContext, Sema};
use super::{FunctionInfo, MethodInfo};
use crate::declaration_validation::{
    AccessorBodyVerdict, AccessorExitForm, AccessorParameterForm, AccessorReceiverForm,
    AccessorYieldRootForm,
};
use crate::path_norm::{mangle_symbol_component, normalize_module_path};
use crate::types::StructField;
use crate::types::{EnumDef, EnumId, StructDef, StructId, Type, TypeKind};

impl<'a, D: DeclarationPhase> Sema<'a, D> {
    /// Whether directives contain `@allow` for a specific warning name.
    /// This is a pure RIR/interner read and is available in every declaration phase.
    pub(crate) fn has_allow_directive<'r>(
        &self,
        mut directives: impl Iterator<Item = rue_rir::RirDirectiveView<'r>>,
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.interner.get("allow");
        let warning_sym = self.interner.get(warning_name);
        directives.any(|directive| {
            Some(directive.name) == allow_sym
                && directive.args.iter().any(|arg| Some(*arg) == warning_sym)
        })
    }

    pub(crate) fn source_function_name(&self, internal_name: Spur) -> Spur {
        self.function_source_names
            .get(&internal_name)
            .copied()
            .unwrap_or(internal_name)
    }

    /// The internal symbol of an ordinary function, derived only from its own
    /// declaration and module (RUE-1125).
    ///
    /// A function's identity is a property of where it is declared, never of
    /// what else the program happens to contain: the spelling is decided from
    /// `(module, source name)` alone, so adding or removing an unrelated
    /// declaration anywhere else cannot restamp an existing function's
    /// semantic identity, body artifact, or dependency edges. Two declarations
    /// name themselves instead, and each is recognized from its own
    /// declaration, never from what the rest of the program contains:
    ///
    /// - The ROOT module's `main` keeps the unmangled `main` symbol the runtime
    ///   `_start`/`__main` glue calls (spec 6.1:38, RUE-920/RUE-921). When no
    ///   root is designated (single-file callers that never call
    ///   `set_root_file_id`) that lone file is the root, so its `main` keeps the
    ///   bare name too. A `main` in any OTHER module is an ordinary namespaced
    ///   function: it must never keep the bare `main` symbol, so it cannot
    ///   collide with the root entry point and so a root that lacks `main` still
    ///   fails with `NoMainFunction` instead of silently adopting an imported
    ///   entry point.
    ///
    /// - An `extern "C"` foreign declaration (ADR-0064) declares an external C
    ///   symbol rather than a Rue callable: it has no body, no artifact, and no
    ///   Rue namespace of its own, and a call site lowers to that raw symbol for
    ///   the linker to satisfy from an archive. Its internal symbol therefore is
    ///   the C name it declares. The declaration is read from its own module, so
    ///   this stays a property of the declaration.
    ///
    /// A `pub extern "C" fn` export is *not* an exception here. Its native body
    /// is an ordinary module-qualified callable like any other; its unmangled C
    /// symbol is specified separately and emitted as a distinct entry thunk
    /// (ADR-0064 P4, `backend::generate_export_thunk_objects`).
    pub(super) fn internal_function_name(&self, source_name: Spur, file_id: FileId) -> Spur {
        let source = self.interner.resolve(&source_name);
        let file_is_root = self.root_file_id.is_none_or(|root| root == file_id);
        if source == "main" && file_is_root {
            return source_name;
        }
        if self.declares_foreign_function(source_name, file_id) {
            return source_name;
        }
        let module_component = self
            .get_symbol_path(file_id)
            .map(normalize_module_path)
            .unwrap_or_else(|| format!("file{}", file_id.index()));
        let module_component = mangle_symbol_component(&module_component);
        self.interner
            .get_or_intern(&format!("__rue_fn_{}__{}", module_component, source))
    }

    /// Whether `file_id` declares `source_name` as an `extern "C"` foreign
    /// function. This reads only that file's own declarations — the same
    /// file-scoped free-function selection body binding performs.
    fn declares_foreign_function(&self, source_name: Spur, file_id: FileId) -> bool {
        self.declaration_index
            .first_free_function(source_name, Some(file_id))
            .is_some_and(|declaration| {
                matches!(
                    self.rir.get(declaration).data,
                    InstData::FnDecl {
                        is_extern: true,
                        ..
                    }
                )
            })
    }

    /// Reject an `extern "C"` foreign declaration of `main`, the program entry
    /// symbol (RUE-1220, spec 9.3:6).
    ///
    /// A foreign declaration names the C symbol it declares (RUE-1125), so
    /// `extern "C" { fn main(); }` takes the bare `main` symbol the runtime
    /// start glue calls (spec 6.1:38). There is no external function for it to
    /// describe: in a non-root module the declaration silently binds the
    /// program's own entry point, so a call through it recurses into `main`,
    /// and in the root module it collides with the entry point's definition.
    /// The rule is the import-side mirror of E1106's rejection of a
    /// `pub extern "C" fn` export named `main`, and applies in every module and
    /// for every signature — agreement with the entry point is not a defence,
    /// which is why RUE-1218's signature check cannot express it.
    ///
    /// This lives in the predeclaration sweep rather than at signature
    /// installation (where the query layer's foreign-redeclaration conflict
    /// check runs) because
    /// it reads only the declaration's own name and `extern` marker: no type is
    /// resolved, so the earlier seam is available, and it is the one point both
    /// producers pass for every declaration the program contains (installation
    /// is reachable only from an already-swept `DeclarationShells`). Rejecting
    /// here means the colliding `main` key is never handed out at all.
    pub(super) fn check_foreign_entry_point_declaration(
        &self,
        declaration: InstRef,
    ) -> CompileResult<()> {
        let inst = self.rir.get(declaration);
        let InstData::FnDecl {
            name,
            is_extern: true,
            ..
        } = &inst.data
        else {
            return Ok(());
        };
        if self.interner.resolve(name) != "main" {
            return Ok(());
        }
        // Without `c_ffi` the declaration is rejected as E1100 by the gate that
        // owns every `extern "C"` form; reporting a rule about a form the
        // program cannot name yet would bury it. Same layering as the accessor
        // sweep, and the same order the driver already produces.
        if !self.preview_features.contains(&PreviewFeature::CFfi) {
            return Ok(());
        }
        // One declaration is the whole error, so the declaration's own span
        // carries it: no second site to label, unlike the redeclaration rule.
        Err(
            CompileError::new(ErrorKind::ForeignEntryPointDeclaration, inst.span)
                .with_note(
                    "a foreign declaration names the C symbol it declares, so this one resolves \
                     to the program's entry point rather than to anything external; the entry \
                     symbol belongs to the runtime start glue (`_start`/`__main`)",
                )
                .with_help(
                    "give the foreign function a different C name, or call the Rue function \
                     directly instead of declaring it foreign",
                ),
        )
    }

    /// Resolve a source-level function name only in the given source file.
    ///
    /// This is the sole source-level lookup path for unqualified calls.
    pub(crate) fn resolve_function_name_local(
        &self,
        source_name: Spur,
        file_id: FileId,
    ) -> Option<Spur> {
        self.functions_by_file_name
            .get(&(file_id, source_name))
            .copied()
    }

    pub(crate) fn resolve_const_info_in_file(
        &self,
        name: Spur,
        file_id: FileId,
    ) -> Option<&ConstInfo> {
        self.value_const(&(file_id, name))
    }

    pub(crate) fn resolve_builtin_struct_name(&self, name: Spur) -> Option<StructId> {
        self.builtin_structs
            .get(&name)
            .copied()
            .filter(|id| self.type_pool.struct_def(*id).is_builtin)
    }

    pub(crate) fn resolve_builtin_enum_name(&self, name: Spur) -> Option<EnumId> {
        self.builtin_enums.get(&name).copied().filter(|id| {
            Some(*id) == self.builtin_arch_id
                || Some(*id) == self.builtin_os_id
                || Some(*id) == self.builtin_data_model_id
        })
    }

    /// Build the demand-population [`InferenceContext`] for constraint
    /// generation (RUE-1091 slice r5b).
    ///
    /// Formerly this eagerly converted every function/method signature and
    /// projected every struct/enum/const family into owned `HashMap`s — an
    /// O(universe) pass paid before any body was analyzed (and, on the
    /// per-body-query hot path, paid afresh for every reached body). It now
    /// returns a context whose signature caches start empty and materialize on
    /// first consult through [`HostInferenceFacts`], so only the signatures and
    /// types a body actually names are converted. The only eager work retained
    /// is a small snapshot of the generated-nominal overlays, which must be
    /// frozen at this point so a nominal generated while analyzing a later body
    /// cannot retroactively perturb an earlier body's type resolution.
    pub fn build_inference_context(&self) -> InferenceContext {
        InferenceContext::new(self)
    }
    /// Check if a directive list contains the @copy directive
    pub(crate) fn has_copy_directive<'r>(
        &self,
        directives: impl Iterator<Item = rue_rir::RirDirectiveView<'r>>,
    ) -> bool {
        let copy_sym = self.interner.get("copy");
        for directive in directives {
            if Some(directive.name) == copy_sym {
                return true;
            }
        }
        false
    }

    /// Check if a directive list carries the `@repr(c)` guarantee marker
    /// (ADR-0064 Amendment 1). The parser has already validated that a `@repr`
    /// directive on a struct carries exactly the argument `c`, so presence of a
    /// `@repr` directive naming `c` is the marker.
    pub(crate) fn has_repr_c_directive<'r>(
        &self,
        directives: impl Iterator<Item = rue_rir::RirDirectiveView<'r>>,
    ) -> bool {
        let repr_sym = self.interner.get("repr");
        let c_sym = self.interner.get("c");
        for directive in directives {
            if Some(directive.name) == repr_sym
                && directive.args.iter().any(|arg| Some(*arg) == c_sym)
            {
                return true;
            }
        }
        false
    }

    /// Post-collection check: a value constant's name must not collide with a
    /// function, struct, or enum (spec 10.3:1, 10.5:1, RUE-239).
    ///
    /// This remains a semantic check because only initializer evaluation can
    /// distinguish value constants from per-file module bindings.
    pub(crate) fn check_const_cross_kind_collisions(&self) -> CompileResult<()> {
        for ((file_id, name), info) in self.value_consts() {
            if self.functions_by_file_name.contains_key(&(*file_id, *name))
                || self.structs_by_file_name.contains_key(&(*file_id, *name))
                || self.enums_by_file_name.contains_key(&(*file_id, *name))
            {
                let name_str = self.interner.resolve(name);
                let kind =
                    crate::declaration_validation::const_cross_kind_collision(name_str, true, true)
                        .expect(
                            "a present non-const declaration collides with this value constant",
                        );
                return Err(CompileError::new(kind, info.span));
            }
        }
        Ok(())
    }

    /// Whether every indexed source free function has a resolved signature.
    pub(crate) fn source_free_function_signatures_are_complete(&self) -> bool {
        self.declaration_index
            .all_free_functions()
            .iter()
            .all(|&declaration| {
                let inst = self.rir.get(declaration);
                let InstData::FnDecl { name, .. } = inst.data else {
                    unreachable!("free-function index contains only FnDecl instructions")
                };
                self.resolve_function_name_local(name, inst.span.file_id)
                    .is_some_and(|key| self.functions.contains_key(&key))
            })
    }
}

impl<'a> Sema<'a, super::MutableDeclarations> {
    /// Phase 1: Register all type names (enum and struct IDs).
    ///
    /// This creates name → ID mappings for all enums and structs in a single pass,
    /// allowing types to reference each other in any order. Struct definitions are
    /// created with placeholder empty fields that will be filled in during phase 2.
    pub(crate) fn register_type_names(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::EnumDecl {
                    is_pub,
                    name,
                    variants,
                    ..
                } => {
                    let enum_name = self.interner.resolve(&*name).to_string();

                    let key = (inst.span.file_id, *name);

                    let variants = self.rir.enum_variants(variants);

                    let mut seen_variants: HashSet<Spur> = HashSet::new();
                    for variant in &variants {
                        if !seen_variants.insert(*variant) {
                            if self.synthetic_declaration_discovery {
                                return Err(CompileError::new(
                                    ErrorKind::DuplicateVariant {
                                        enum_name: enum_name.clone(),
                                        variant_name: self.interner.resolve(&*variant).to_owned(),
                                    },
                                    inst.span,
                                ));
                            }
                            // Query-owned production payloads validate this
                            // signature before AIR completion. Shell import is
                            // intentionally permissive because it precedes
                            // keyed payload projection.
                        }
                    }

                    // Convert variant symbols to strings
                    let variant_names: Arc<[Arc<str>]> = variants
                        .iter()
                        .map(|v| Arc::from(self.interner.resolve(&*v)))
                        .collect();

                    let enum_def = EnumDef {
                        name: Arc::from(enum_name.as_str()),
                        variants: variant_names,
                        // Payload types are resolved in phase 2
                        // (`resolve_enum_payloads`), once all type names are
                        // registered, so payloads may reference any struct/enum.
                        variant_payloads: Vec::new(),
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based EnumId
                    let (enum_id, _) = self.type_pool.declare_enum(*name, enum_def);

                    // Source declarations are always keyed by their defining file.
                    self.enums_by_file_name.insert(key, enum_id);
                }
                InstData::StructDecl {
                    directives,
                    is_pub,
                    is_linear,
                    name,
                    ..
                } => {
                    let struct_name = self.interner.resolve(&*name).to_string();

                    let key = (inst.span.file_id, *name);

                    let directives = self.rir.directives(directives);
                    let is_copy = self.has_copy_directive(directives.iter());
                    let is_repr_c = self.has_repr_c_directive(directives.iter());

                    if *is_linear && is_copy {
                        if self.synthetic_declaration_discovery {
                            return Err(CompileError::new(
                                ErrorKind::LinearStructCopy(struct_name.clone()),
                                inst.span,
                            ));
                        }
                        // Query-owned production payloads validate this before
                        // AIR completion. The shell phase itself runs before
                        // keyed payload projection.
                    }

                    // Create placeholder struct def (fields will be resolved in phase 2)
                    let struct_def = StructDef {
                        name: Arc::from(struct_name.as_str()),
                        fields: Vec::new(), // Filled in during resolve_declarations
                        is_copy,
                        is_linear: *is_linear,
                        destructor: None,  // Filled in during resolve_declarations
                        is_builtin: false, // User-defined struct
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based StructId
                    let (struct_id, _) = self.type_pool.declare_struct(*name, struct_def);
                    // Record the `@repr(c)` guarantee marker on the pool as a
                    // side fact (ADR-0064 Amendment 1). Preview gating and the
                    // reject-don't-guess eligibility check run in phase 2
                    // (`validate_repr_c_struct`), once fields are resolved.
                    if is_repr_c {
                        self.type_pool.set_struct_repr_c(struct_id);
                    }
                    if self
                        .trusted_standard_library_files
                        .contains(&inst.span.file_id)
                        && let Some(module_path) = self.symbol_paths.get(&inst.span.file_id)
                        && let Some(lang_item) = crate::LangItem::from_standard_library_nominal(
                            module_path,
                            &self
                                .type_pool
                                .struct_metadata(struct_id)
                                .expect("newly declared struct must retain its shell")
                                .name,
                        )
                    {
                        self.type_pool.set_struct_lang_item(struct_id, lang_item);
                    }

                    // Source declarations are always keyed by their defining file.
                    self.structs_by_file_name.insert(key, struct_id);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Phase 2: Resolve all declarations.
    ///
    /// Now that all type names are registered, this resolves:
    /// - Struct field types (must be done first for @copy validation)
    /// - @copy struct validation, destructors, functions, and methods
    ///
    /// # Array Type Registration
    ///
    /// Array types from explicit type annotations (struct fields, function parameters,
    /// return types, local variable annotations) are registered during this phase via
    /// `resolve_type()` calls. Array types from literals (inferred during HM inference)
    /// are created on-demand via the thread-safe `TypeInternPool` during function
    /// body analysis.
    #[cfg(test)]
    pub(crate) fn resolve_declarations(&mut self) -> CompileResult<()> {
        self.resolve_declarations_for_test()
    }

    pub(crate) fn resolve_declarations_for_test(&mut self) -> CompileResult<()> {
        debug_assert!(!self.declaration_binding_active);
        self.declaration_binding_active = true;
        let result = (|| {
            self.validate_indexed_const_declarations()?;
            self.resolve_struct_fields()?;
            self.resolve_enum_payloads()?;
            // Reject infinite-size types now that every struct field and enum
            // payload is resolved, before any later pass (layout, drop-glue)
            // recurses through the field graph and overflows the host stack
            // (RUE-264).
            self.check_recursive_value_types()?;
            self.resolve_remaining_declarations()?;
            // Destructor discovery can change drop facts without changing the
            // already-proven-acyclic graph. Refresh the same canonical metadata
            // before deferred ownership gates and body analysis query it.
            self.type_pool
                .finalize_containment_metadata()
                .expect("destructor collection cannot introduce containment cycles");
            self.validate_deferred_ownership_gates()?;
            // Now that value constants are separated from module bindings, reject
            // any value-constant name that collides with a function/struct/enum
            // (order-independent E0436, spec 10.3:1/10.5:1, RUE-239).
            self.check_const_cross_kind_collisions()?;
            self.capture_resolved_declaration_type_dependencies();
            self.declaration_type_observer = None;
            Ok(())
        })();
        self.declaration_binding_active = false;
        debug_assert!(self.const_resolution_in_progress.is_empty());
        if result.is_ok() {
            debug_assert!(
                self.source_free_function_signatures_are_complete(),
                "BoundSema requires every indexed source free-function signature"
            );
            debug_assert!(self.fn_signatures_in_flight.is_empty());
        }
        result
    }

    pub(super) fn capture_resolved_declaration_type_dependencies(&mut self) {
        use super::{
            DeclarationTypeDependencyKind as Edge, DeclarationTypeDependencySourceKind as Source,
        };
        let structs = self
            .structs_by_file_name
            .iter()
            .map(|((_, name), id)| (*name, *id))
            .collect::<Vec<_>>();
        for (name, id) in structs {
            let def = self.type_pool.struct_def(id);
            for field in &def.fields {
                self.capture_declaration_type(
                    def.file_id,
                    self.interner.resolve(&name).to_string(),
                    None,
                    Source::Struct,
                    Edge::Field,
                    field.ty,
                );
            }
        }
        let enums = self
            .enums_by_file_name
            .iter()
            .map(|((_, name), id)| (*name, *id))
            .collect::<Vec<_>>();
        for (name, id) in enums {
            let def = self.type_pool.enum_def(id);
            for &ty in def.variant_payloads.iter().flatten() {
                self.capture_declaration_type(
                    def.file_id,
                    self.interner.resolve(&name).to_string(),
                    None,
                    Source::Enum,
                    Edge::Payload,
                    ty,
                );
            }
        }
        let functions = self
            .functions
            .iter()
            .map(|(name, info)| (*name, *info))
            .collect::<Vec<_>>();
        for (name, info) in functions {
            let source_name = self
                .interner
                .resolve(&self.source_function_name(name))
                .to_string();
            let mut types = self.param_arena.types(info.params).to_vec();
            types.push(info.return_type);
            for ty in types {
                self.capture_declaration_type(
                    info.file_id,
                    source_name.clone(),
                    None,
                    Source::Function,
                    Edge::Signature,
                    ty,
                );
            }
        }
        let methods = self
            .methods
            .iter()
            .map(|(key, info)| (*key, *info))
            .collect::<Vec<_>>();
        for ((struct_id, method_name), info) in methods {
            // Membership, not the generated-name prefix (RUE-1050).
            if self.anonymous_struct_ids.contains(&struct_id) {
                continue;
            }
            let owner = self.type_pool.struct_def(struct_id);
            let source_kind = if info.has_self {
                Source::Method
            } else {
                Source::AssociatedFunction
            };
            self.capture_declaration_type(
                info.span.file_id,
                self.interner.resolve(&method_name).to_string(),
                Some(owner.name.to_string()),
                source_kind,
                Edge::Owner,
                Type::new_struct(struct_id),
            );
            let mut types = self.param_arena.types(info.params).to_vec();
            types.push(info.return_type);
            for ty in types {
                self.capture_declaration_type(
                    info.span.file_id,
                    self.interner.resolve(&method_name).to_string(),
                    Some(owner.name.to_string()),
                    source_kind,
                    Edge::Signature,
                    ty,
                );
            }
        }
        let constants = self
            .value_consts()
            .map(|((file, name), info)| (*file, *name, info.ty))
            .collect::<Vec<_>>();
        for (file, name, ty) in constants {
            self.capture_declaration_type(
                file,
                self.interner.resolve(&name).to_string(),
                None,
                Source::ValueConst,
                Edge::DeclaredType,
                ty,
            );
        }
        let destructors = self.declaration_index.destructors().to_vec();
        for destructor in destructors {
            if let Some(id) = self
                .structs_by_file_name
                .get(&(destructor.span.file_id, destructor.type_name))
                .copied()
            {
                self.capture_declaration_type(
                    destructor.span.file_id,
                    self.interner.resolve(&destructor.type_name).to_string(),
                    Some(self.interner.resolve(&destructor.type_name).to_string()),
                    Source::Destructor,
                    Edge::Owner,
                    Type::new_struct(id),
                );
            }
        }
    }

    fn capture_declaration_type(
        &mut self,
        source_file: FileId,
        source_name: String,
        source_owner_name: Option<String>,
        source_kind: super::DeclarationTypeDependencySourceKind,
        dependency_kind: super::DeclarationTypeDependencyKind,
        ty: Type,
    ) {
        let target = match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self
                    .type_pool
                    .struct_metadata(id)
                    .expect("declaration dependency names a registered struct identity");
                // Membership, not the generated-name prefix (RUE-1050).
                if def.is_builtin || self.anonymous_struct_ids.contains(&id) {
                    return;
                }
                Some((
                    def.file_id,
                    def.name,
                    super::DeclarationTypeDependencyTargetKind::Struct,
                ))
            }
            TypeKind::Enum(id) => {
                let def = self
                    .type_pool
                    .enum_metadata(id)
                    .expect("declaration dependency names a registered enum identity");
                Some((
                    def.file_id,
                    def.name,
                    super::DeclarationTypeDependencyTargetKind::Enum,
                ))
            }
            TypeKind::Array(id) => {
                self.capture_declaration_type(
                    source_file,
                    source_name,
                    source_owner_name,
                    source_kind,
                    dependency_kind,
                    self.type_pool.array_def(id).0,
                );
                return;
            }
            TypeKind::PtrConst(id) => {
                self.capture_declaration_type(
                    source_file,
                    source_name,
                    source_owner_name,
                    source_kind,
                    dependency_kind,
                    self.type_pool.ptr_const_def(id),
                );
                return;
            }
            TypeKind::PtrMut(id) => {
                self.capture_declaration_type(
                    source_file,
                    source_name,
                    source_owner_name,
                    source_kind,
                    dependency_kind,
                    self.type_pool.ptr_mut_def(id),
                );
                return;
            }
            _ => None,
        };
        if let Some((target_file, target_name, target_kind)) = target {
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
                target_name: target_name.to_string(),
                target_kind,
            };
            if self.declaration_type_dependency_index.insert(event.clone()) {
                self.declaration_type_dependencies.push(event);
            }
            self.body_analysis_work.declaration_type_dependency_events += 1;
        }
    }

    /// Phase 2: Resolve tuple-variant payload types (RUE-221, ADR-0038).
    ///
    /// Runs after all type names are registered, so payloads may reference any
    /// struct or enum regardless of declaration order. Decodes the
    /// self-describing payload region stored in the RIR `EnumDecl`
    /// (`[k, t0, ..., t_{k-1}]` per variant) into concrete `Type`s and updates
    /// the registered [`EnumDef`].
    pub(crate) fn resolve_enum_payloads(&mut self) -> CompileResult<()> {
        // Collect the work first to avoid borrowing `self.rir` while mutating
        // the type pool through `self`.
        let mut jobs: Vec<(Spur, InstRef, Span)> = Vec::new();
        for (inst_ref, inst) in self.rir.iter() {
            if let InstData::EnumDecl { name, .. } = &inst.data {
                jobs.push((*name, inst_ref, inst.span));
            }
        }

        for (name, declaration, span) in jobs {
            let InstData::EnumDecl {
                payloads, variants, ..
            } = &self.rir.get(declaration).data
            else {
                unreachable!()
            };
            let payload_symbols: Vec<Vec<Spur>> = self
                .rir
                .enum_payloads(payloads, variants)
                .map(|payload| payload.to_vec())
                .collect();
            let source_name = self.interner.resolve(&name).to_string();
            self.declaration_type_observer =
                (payload_symbols.iter().any(|payload| !payload.is_empty())
                    && !source_name.starts_with("__anon_enum_"))
                .then_some((
                    span.file_id,
                    source_name,
                    None,
                    super::DeclarationTypeDependencySourceKind::Enum,
                    super::DeclarationTypeDependencyKind::Payload,
                ));
            // Decode per-variant payload type symbols.
            let mut variant_payloads: Vec<Vec<Type>> = Vec::new();
            for symbols in payload_symbols {
                let mut payload = Vec::with_capacity(symbols.len());
                for ty_sym in symbols {
                    // A slice type `[T]` is second-class (ADR-0037, ADR-0043,
                    // RUE-322): an enum payload is aggregate storage, so it
                    // cannot hold a fat-pointer view (E0488).
                    self.reject_slice_escape(ty_sym, span, ErrorKind::SliceInAggregateField)?;
                    let ty = self.resolve_type(ty_sym, span)?;
                    // A payload of type `type` cannot exist at runtime
                    // (spec 4.14:6); reject it like struct fields do.
                    if ty.is_comptime_type() {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "type values cannot exist at runtime".to_string(),
                            },
                            span,
                        ));
                    }
                    payload.push(ty);
                }
                variant_payloads.push(payload);
            }

            let enum_id = *self
                .enums_by_file_name
                .get(&(span.file_id, name))
                .expect("enum registered in phase 1");
            let metadata = self
                .type_pool
                .enum_declaration_metadata(enum_id)
                .expect("enum registered as a declaration shell in phase 1");
            let def = EnumDef {
                name: metadata.name,
                variants: metadata.variants,
                variant_payloads,
                is_pub: metadata.is_pub,
                file_id: metadata.file_id,
            };
            self.type_pool.complete_declared_enum(enum_id, def);
        }
        Ok(())
    }

    /// Resolve struct field types. Must run before @copy validation.
    pub(crate) fn resolve_struct_fields(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl { name, fields, .. } = &inst.data {
                let name_str = self.interner.resolve(&*name).to_string();
                // Get the struct ID from the lookup table
                let struct_id = *self
                    .structs_by_file_name
                    .get(&(inst.span.file_id, *name))
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::InternalError(
                                ice!(
                                    "struct not found in structs map",
                                    phase: "sema/declarations",
                                    details: {
                                        "struct_name" => name_str.to_string()
                                    }
                                )
                                .to_string(),
                            ),
                            inst.span,
                        )
                    })?;

                let fields = self.rir.struct_fields(fields);

                let mut seen_fields: HashSet<Spur> = HashSet::new();
                for (field_name, _) in fields.values() {
                    if !seen_fields.insert(field_name) {
                        if self.synthetic_declaration_discovery {
                            return Err(CompileError::new(
                                ErrorKind::DuplicateField {
                                    struct_name: name_str.clone(),
                                    field_name: self.interner.resolve(&field_name).to_owned(),
                                },
                                inst.span,
                            ));
                        }
                        panic!(
                            "keyed struct signature validation must reject duplicate fields before AIR installation"
                        );
                    }
                }

                // Resolve field types
                self.declaration_type_observer = (!name_str.starts_with("__anon_struct_"))
                    .then_some((
                        inst.span.file_id,
                        name_str.clone(),
                        None,
                        super::DeclarationTypeDependencySourceKind::Struct,
                        super::DeclarationTypeDependencyKind::Field,
                    ));
                let mut resolved_fields = Vec::new();
                for (field_name, field_type) in fields.values() {
                    // A slice type `[T]` is second-class (ADR-0037, ADR-0043,
                    // RUE-322): storing it in a struct field would let the
                    // fat-pointer view escape its borrow's scope (E0488).
                    self.reject_slice_escape(
                        field_type,
                        inst.span,
                        ErrorKind::SliceInAggregateField,
                    )?;
                    let field_ty = self.resolve_type(field_type, inst.span)?;
                    // spec 4.14:6 — type values cannot exist at runtime. A
                    // struct field of type `type` is a runtime storage slot for
                    // a type value, which is forbidden; reject it at the
                    // declaration rather than letting a `Holder { t: i32 }`
                    // literal ICE in codegen ("block has no terminator",
                    // RUE-217). Mirrors the clean E1200 that
                    // `let t = comptime { i32 };` already produces.
                    if field_ty.is_comptime_type() {
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "type values cannot exist at runtime".to_string(),
                            },
                            inst.span,
                        ));
                    }
                    resolved_fields.push(StructField {
                        name: self.interner.resolve(&field_name).to_string(),
                        ty: field_ty,
                    });
                }

                // Update the struct definition in the pool with resolved fields
                let metadata = self
                    .type_pool
                    .struct_declaration_metadata(struct_id)
                    .expect("struct registered as a declaration shell in phase 1");
                let struct_def = StructDef {
                    name: metadata.name,
                    fields: resolved_fields,
                    is_copy: metadata.is_copy,
                    is_linear: metadata.is_linear,
                    destructor: metadata.destructor,
                    is_builtin: metadata.is_builtin,
                    is_pub: metadata.is_pub,
                    file_id: metadata.file_id,
                };
                self.type_pool
                    .complete_declared_struct(struct_id, struct_def);
            }
        }
        Ok(())
    }

    /// Reject types whose size is infinite because they contain themselves by
    /// value (RUE-264). A well-formed type must have a finite layout: a struct
    /// field, enum payload, or array element stores its value *inline*, so a
    /// type that transitively contains itself through only such by-value edges
    /// has no finite size. Such a definition would otherwise recurse forever in
    /// layout / drop-glue computation and overflow the compiler's own stack
    /// (a SIGABRT that even bypasses the ICE panic-hook). This is the
    /// type-graph analogue of the constant-initializer cycle check (E0461):
    /// it detects a cycle in the "contains by value" graph and reports E0483
    /// (cf. Rust's E0072).
    ///
    /// A pointer field (`ptr const T` / `ptr mut T`) is indirection, not
    /// by-value containment, so it breaks the cycle and is allowed — that is
    /// how a recursive data structure is written.
    pub(crate) fn check_recursive_value_types(&mut self) -> CompileResult<()> {
        let explicitly_linear: HashSet<_> = self
            .type_pool
            .all_struct_ids()
            .into_iter()
            .filter(|&id| self.type_pool.struct_def(id).is_linear)
            .collect();
        if let Err(cycle) = self.type_pool.finalize_containment_metadata() {
            // Drive diagnostics from the source declaration so the canonical
            // graph remains span-free while E0483 retains its exact span.
            for (_, inst) in self.rir.iter() {
                let (name_sym, span) = match &inst.data {
                    InstData::StructDecl { name, .. } | InstData::EnumDecl { name, .. } => {
                        (*name, inst.span)
                    }
                    _ => continue,
                };
                let key = (span.file_id, name_sym);
                let start_ty = if let Some(&struct_id) = self.structs_by_file_name.get(&key) {
                    Type::new_struct(struct_id)
                } else if let Some(&enum_id) = self.enums_by_file_name.get(&key) {
                    Type::new_enum(enum_id)
                } else {
                    continue;
                };
                if start_ty != cycle.root {
                    continue;
                }
                let name = self.interner.resolve(&name_sym).to_string();
                return Err(CompileError::new(
                    ErrorKind::RecursiveTypeInfiniteSize {
                        name,
                        cycle: cycle.path.join(" -> "),
                    },
                    span,
                ));
            }
            unreachable!("containment cycle root must be a source nominal declaration");
        }

        // The canonical pass has propagated infectious linearity. Preserve the
        // first declaration-order causing field for the existing diagnostic
        // note without maintaining a second propagation authority.
        for struct_id in self.type_pool.all_struct_ids() {
            if explicitly_linear.contains(&struct_id) {
                continue;
            }
            let def = self.type_pool.struct_def(struct_id);
            if !def.is_linear {
                continue;
            }
            let Some(cause) = def
                .fields
                .iter()
                .find(|field| self.type_pool.type_carries_linear(field.ty))
            else {
                continue;
            };
            let cause = (cause.name.clone(), self.format_type_name(cause.ty));
            self.infectious_linear.insert(struct_id, cause);
        }
        Ok(())
    }

    /// Resolve @copy validation, destructors, functions, and methods.
    pub(crate) fn resolve_remaining_declarations(&mut self) -> CompileResult<()> {
        // First pass: collect all declarations and validate @copy structs
        for (inst_ref, inst) in self.rir.iter() {
            match &inst.data {
                InstData::StructDecl {
                    directives,
                    name,
                    methods,
                    ..
                } => {
                    self.validate_copy_struct(directives, *name, inst.span)?;
                    // Collect methods defined inline in the struct
                    self.collect_struct_methods(*name, methods, inst.span)?;
                }

                InstData::DropFnDecl { type_name, .. } => {
                    self.collect_destructor(*type_name, inst.span)?;
                }

                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                    ..
                } => {
                    // Reject duplicate parameter names (RUE-349) for EVERY
                    // function/method form. `rir.iter()` visits every `FnDecl`
                    // in the arena exactly once — free functions, named-struct
                    // methods, associated functions, and anonymous-struct
                    // methods alike — so checking here before the
                    // signature-collection skips below covers them all in one
                    // place without double-reporting.
                    self.check_duplicate_param_names(params)?;

                    // Skip methods (has_self = true) - these are handled elsewhere:
                    // - Named struct methods are collected via ImplDecl
                    if *has_self {
                        continue;
                    }

                    // Skip ALL methods from anonymous structs (including associated functions)
                    // These are registered during comptime evaluation with proper Self type context
                    if self.declaration_index.is_anonymous_method(inst_ref) {
                        continue;
                    }

                    // Skip named-struct associated functions (no `self`): they are
                    // collected via `collect_struct_methods` with `Self` bound to
                    // the enclosing type (RUE-123). Collecting them here as free
                    // functions would reject a `Self` signature type.
                    if self.declaration_index.is_named_method(inst_ref) {
                        continue;
                    }
                    self.collect_function_signature(
                        inst_ref,
                        *name,
                        *return_type,
                        *body,
                        inst.span,
                        *is_pub,
                        *is_unchecked,
                        *is_extern,
                        *is_c_export,
                    )?;
                }

                InstData::ConstDecl { name, .. } => {
                    // May already be collected: another constant's
                    // initializer can pull declarations in early (the
                    // collector is dependency-ordered, RUE-171).
                    if self
                        .bound_const_candidates
                        .as_ref()
                        .expect(
                            "const candidate authority must be installed before declaration resolution",
                        )
                        .contains(inst_ref)
                    {
                        self.collect_const_by_key((inst.span.file_id, *name))?;
                    }
                }

                _ => {}
            }
        }

        // Second pass: validate `@repr(c)` structs now that every destructor has
        // been collected, so the `c_ffi_safe` reject-list sees a
        // destructor-bearing struct (or field) uniformly regardless of
        // declaration order.
        for (_inst_ref, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                directives, name, ..
            } = &inst.data
            {
                self.validate_repr_c_struct(directives, *name, inst.span)?;
            }
        }

        Ok(())
    }

    /// Validate that a @copy struct only contains Copy type fields.
    fn validate_copy_struct(
        &self,
        directives: &rue_rir::RirDirectivesRange,
        name: Spur,
        span: Span,
    ) -> CompileResult<()> {
        let directives = self.rir.directives(directives);
        if !self.has_copy_directive(directives.iter()) {
            return Ok(());
        }

        let struct_name = self.interner.resolve(&name).to_string();
        // Get the struct ID from the lookup table
        let struct_id = *self
            .structs_by_file_name
            .get(&(span.file_id, name))
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(
                        ice!(
                            "struct not found during @copy validation",
                            phase: "sema/declarations",
                            details: {
                                "struct_name" => struct_name.clone()
                            }
                        )
                        .to_string(),
                    ),
                    span,
                )
            })?;

        // Get struct definition from the pool
        let struct_def = self.type_pool.struct_def(struct_id);

        for field in &struct_def.fields {
            if !self.is_type_copy(field.ty) {
                let field_type_name = self.format_type_name(field.ty);
                return Err(CompileError::new(
                    ErrorKind::CopyStructNonCopyField(Box::new(CopyStructNonCopyFieldError {
                        struct_name,
                        field_name: field.name.clone(),
                        field_type: field_type_name,
                    })),
                    span,
                ));
            }
        }
        Ok(())
    }

    /// Whether `ty` is a by-value aggregate (struct or fixed array) — the types
    /// the P4 export thunk cannot yet repack across the C boundary.
    fn is_aggregate_type(ty: Type) -> bool {
        matches!(ty.kind(), TypeKind::Struct(_) | TypeKind::Array(_))
    }

    /// Enforce one `extern "C"` parameter or return type (ADR-0064 P1 +
    /// Amendment 1).
    ///
    /// ## Diagnostic layering
    ///
    /// P3 (RUE-1057) widens the by-value type set to **C-classifiable
    /// aggregates**: a `@repr(c)` struct satisfying `c_passable_by_value` (marker
    /// eligible: `has_c_layout` ∧ `c_ffi_safe`) now crosses by value, joining the
    /// full scalar set P2 accepts. Amendment 1's marker/array rules still layer
    /// on top. The order below picks the most specific, forward-looking
    /// diagnostic:
    ///
    /// 1. A **fixed array** as a direct parameter/return is the array-decay
    ///    rejection (`ExternArrayByValue`) — C has no by-value array parameter;
    ///    arrays are eligible only as struct fields.
    /// 2. A **struct** without `@repr(c)` names the missing marker
    ///    (`ExternAggregateNotReprC`). A marked-and-eligible `@repr(c)` struct is
    ///    now *accepted* by value (P3); a marked struct that is not marker
    ///    eligible was already rejected at its declaration, so reaching the phase
    ///    diagnostic here would be a marker the declaration let through — mapped
    ///    to `ExternSignatureTypeUnsupported` defensively.
    /// 3. Any remaining type that is not passable in this phase (enums, and — once
    ///    RUE-714 adds them — floats) takes the phase diagnostic via the
    ///    `c_passable_by_value` predicate seam.
    fn check_extern_signature_type(&self, ty: Type, span: Span) -> CompileResult<()> {
        match ty.kind() {
            TypeKind::Array(_) => Err(CompileError::new(
                ErrorKind::ExternArrayByValue {
                    ty: self.format_type_name(ty),
                },
                span,
            )),
            TypeKind::Struct(struct_id) => {
                if !self.type_pool.is_struct_repr_c(struct_id) {
                    return Err(CompileError::new(
                        ErrorKind::ExternAggregateNotReprC {
                            ty: self.format_type_name(ty),
                        },
                        span,
                    ));
                }
                // A marked, eligible `@repr(c)` struct now crosses by value (P3):
                // the target-C classifier packs it eightbyte-wise. Delegating to
                // the predicate keeps the accept/reject boundary in one authority.
                crate::c_passable_by_value(&self.type_pool, ty).map_err(|_| {
                    CompileError::new(
                        ErrorKind::ExternSignatureTypeUnsupported {
                            ty: self.format_type_name(ty),
                        },
                        span,
                    )
                })
            }
            _ => crate::c_passable_by_value(&self.type_pool, ty).map_err(|_| {
                CompileError::new(
                    ErrorKind::ExternSignatureTypeUnsupported {
                        ty: self.format_type_name(ty),
                    },
                    span,
                )
            }),
        }
    }

    /// Gate and validate a `@repr(c)` struct (ADR-0064 Amendment 1, RUE-1063).
    ///
    /// The marker is meaningless without FFI, so it is gated behind the existing
    /// `c_ffi` preview. When present it must satisfy the reject-don't-guess list
    /// *on the marker itself*: no empty body, no enum / non-`@repr(c)` aggregate
    /// / unsupported field, and no linear / destructor-bearing field. Layout is
    /// otherwise unchanged — compact layout already is the C layout for the
    /// supported subset, so the marker changes no bytes today; it is a guarantee.
    fn validate_repr_c_struct(
        &self,
        directives: &rue_rir::RirDirectivesRange,
        name: Spur,
        span: Span,
    ) -> CompileResult<()> {
        let directives = self.rir.directives(directives);
        if !self.has_repr_c_directive(directives.iter()) {
            return Ok(());
        }

        // The marker is inert without the FFI preview: require it explicitly, as
        // an `extern "C"` declaration does.
        self.require_preview(
            rue_error::PreviewFeature::CFfi,
            "the `@repr(c)` representation marker",
            span,
        )?;

        let struct_name = self.interner.resolve(&name).to_string();
        let struct_id = *self
            .structs_by_file_name
            .get(&(span.file_id, name))
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InternalError(
                        ice!(
                            "struct not found during @repr(c) validation",
                            phase: "sema/declarations",
                            details: { "struct_name" => struct_name.clone() }
                        )
                        .to_string(),
                    ),
                    span,
                )
            })?;

        let struct_ty = Type::new_struct(struct_id);
        if let Err(failure) = crate::repr_c_marker_eligible(&self.type_pool, struct_ty) {
            return Err(self.repr_c_ineligible_error(struct_name, &failure, span));
        }
        Ok(())
    }

    /// Render an [`FfiPredicateFailure`](crate::FfiPredicateFailure) from the
    /// `@repr(c)` eligibility check into a user-facing diagnostic, preserving the
    /// failing predicate, field path, and reason (the RUE-504 exposure shape).
    fn repr_c_ineligible_error(
        &self,
        struct_name: String,
        failure: &crate::FfiPredicateFailure,
        span: Span,
    ) -> CompileError {
        let field_path = failure.field_path_display();
        let failing_type = self.format_type_name(failure.failing_type);
        let reason = if field_path.is_empty() {
            failure.reason.describe().to_string()
        } else {
            format!(
                "field `{field_path}` of type `{failing_type}` — {}",
                failure.reason.describe()
            )
        };
        CompileError::new(
            ErrorKind::ReprCStructIneligible(Box::new(rue_error::ReprCIneligibleError {
                struct_name,
                field_path,
                failing_type,
                reason,
            })),
            span,
        )
    }

    /// Collect a destructor definition and register it with its struct.
    pub(super) fn collect_destructor(&mut self, type_name: Spur, span: Span) -> CompileResult<()> {
        let type_name_str = self.interner.resolve(&type_name).to_string();

        // Get the struct ID from the lookup table
        let Some(struct_id) = self
            .structs_by_file_name
            .get(&(span.file_id, type_name))
            .copied()
        else {
            if self.synthetic_declaration_discovery {
                return Err(CompileError::new(
                    crate::declaration_validation::destructor_unknown_type(&type_name_str),
                    span,
                ));
            }
            panic!(
                "keyed destructor signature validation must reject an unknown owner before AIR installation"
            );
        };

        let struct_def = self.type_pool.struct_def(struct_id);
        if struct_def.destructor.is_some() {
            if self.synthetic_declaration_discovery {
                return Err(CompileError::new(
                    crate::declaration_validation::duplicate_destructor(&type_name_str),
                    span,
                ));
            }
            panic!(
                "keyed destructor signature validation must reject duplicate destructors before AIR installation"
            );
        }

        // A @copy struct cannot have a destructor (RUE-159, the spirit of
        // Rust's E0184): copies are implicit and untracked, so every copy
        // would run the destructor again — double cleanup of the same
        // logical resource. `is_copy` is already set here: struct directives
        // are processed in phase 1 (register_type_names), before this pass.
        if struct_def.is_copy {
            let mut err = CompileError::new(
                ErrorKind::CopyStructWithDestructor {
                    type_name: type_name_str,
                },
                span,
            )
            .with_label("destructor defined here", span)
            .with_note(
                "`@copy` values are duplicated implicitly, so the destructor would run \
                 once per copy — cleaning up the same resource multiple times",
            )
            .with_help("remove the `@copy` attribute or remove the `drop fn`");
            if let Some(copy_span) = self.find_copy_directive_span(type_name) {
                err = err.with_label("type declared `@copy` here", copy_span);
            }
            return Err(err);
        }

        let destructor_name = format!("{}.__drop", type_name_str);
        self.type_pool
            .set_struct_destructor(struct_id, destructor_name);
        self.destructor_spans.insert(struct_id, span);
        Ok(())
    }

    /// Find the span of the `@copy` directive on the struct declaration
    /// named `type_name`, for diagnostics that point at the attribute.
    fn find_copy_directive_span(&self, type_name: Spur) -> Option<Span> {
        let copy_sym = self.interner.get("copy")?;
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                name, directives, ..
            } = &inst.data
            {
                if *name != type_name {
                    continue;
                }
                let directives = self.rir.directives(directives);
                return directives
                    .iter()
                    .find(|d| d.name == copy_sym)
                    .map(|d| d.span);
            }
        }
        None
    }

    /// Reject a parameter list that names the same parameter twice, e.g.
    /// `fn f(x: i32, x: i32)` (RUE-349). Without this check the later binding
    /// silently shadows the earlier one on a first-wins basis, so a typo'd or
    /// duplicated name compiles instead of erroring and which parameter a
    /// reference resolves to is an unstated policy. The diagnostic points at
    /// the second (duplicate) occurrence, mirroring the duplicate
    /// pattern-binding check (E0484). `self` is never in this list — a method
    /// receiver is carried separately (`has_self`) — so `fn m(self, a, a)`
    /// still errors on the repeated `a` while `self` plus one distinct param
    /// is fine.
    fn check_duplicate_param_names(&self, params: &rue_rir::RirParamsRange) -> CompileResult<()> {
        let params = self.rir.params(params);
        let names = params
            .iter()
            .map(|param| self.interner.resolve(&param.name))
            .collect::<Vec<_>>();
        if let Some(kind) = crate::declaration_validation::duplicate_parameter(names.iter()) {
            let duplicate = match &kind {
                ErrorKind::DuplicateParameter { name } => name,
                _ => unreachable!("duplicate-parameter rule has one diagnostic"),
            };
            let mut seen = HashSet::new();
            let span = params
                .iter()
                .find(|param| {
                    let name = self.interner.resolve(&param.name);
                    !seen.insert(name) && name == duplicate
                })
                .map(|param| param.span)
                .expect("shared rule identified a present duplicate");
            return Err(CompileError::new(kind, span));
        }
        Ok(())
    }

    /// Collect a function signature for forward reference.
    #[allow(clippy::too_many_arguments)]
    fn collect_function_signature(
        &mut self,
        declaration: InstRef,
        name: Spur,
        return_type_sym: Spur,
        body: InstRef,
        span: Span,
        is_pub: bool,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
    ) -> CompileResult<()> {
        // Reject user functions whose name collides with a runtime/codegen helper
        // symbol (e.g. `__rue_str_eq`, `__rue_alloc`, `_start`). Without this, such a
        // definition either fails to link with a confusing duplicate-symbol error or
        // silently binds calls to the runtime's definition instead of the user's.
        let name_str = self.interner.resolve(&name);
        if let Some(kind) = crate::declaration_validation::reserved_function_name(name_str) {
            return Err(CompileError::new(kind, span));
        }
        self.declaration_type_observer = Some((
            span.file_id,
            name_str.to_string(),
            None,
            super::DeclarationTypeDependencySourceKind::Function,
            super::DeclarationTypeDependencyKind::Signature,
        ));

        // A `-> borrow T` result is the accessor form (ADR-0062), which a free
        // function can never satisfy: it has no receiver to project from.
        check_accessor_declaration_shape(
            self.rir,
            self.interner,
            declaration,
            Some(body),
            false,
            &self.preview_features,
        )?;
        let InstData::FnDecl {
            params: rir_params_range,
            directives: directives_range,
            ..
        } = &self.rir.get(declaration).data
        else {
            unreachable!()
        };
        let params = self.rir.params(rir_params_range);
        let directives = self.rir.directives(directives_range);
        let allow_unused_function = self.has_allow_directive(directives.iter(), "unused_function");
        let allow_unused_variable = self.has_allow_directive(directives.iter(), "unused_variable");
        let allow_unreachable_code =
            self.has_allow_directive(directives.iter(), "unreachable_code");

        let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
        let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();

        // Check if this function has any comptime parameters. Both kinds
        // require per-call-site specialization (RUE-166):
        // - `comptime T: type` -> type parameter (specialized per type)
        // - `comptime n: i32` -> value parameter (specialized per value, so
        //   the body sees `n` as a compile-time constant)
        let type_sym = self.interner.get_or_intern("type");
        let is_generic = params.iter().any(|p| p.is_comptime);

        // Collect type parameter names (comptime parameters whose type is `type`)
        let type_param_names: Vec<Spur> = params
            .iter()
            .filter(|p| p.is_comptime && p.ty == type_sym)
            .map(|p| p.name)
            .collect();

        // Collect comptime VALUE parameter names (comptime params whose type is
        // not `type`, e.g. `comptime N: i32`). A runtime parameter whose type
        // uses one as an array length (`arr: [i32; N]`) must be deferred to
        // specialization, when N's concrete value is known (RUE-16).
        let value_param_names: Vec<Spur> = params
            .iter()
            .filter(|p| p.is_comptime && p.ty != type_sym)
            .map(|p| p.name)
            .collect();
        let value_param_type_syms: Vec<(Spur, Spur)> = params
            .iter()
            .filter(|p| p.is_comptime && p.ty != type_sym)
            .map(|p| (p.name, p.ty))
            .collect();

        // For generic functions, we defer type resolution of type parameters until specialization.
        // We use Type::COMPTIME_TYPE as a placeholder for comptime T: type parameters.
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| {
                if p.is_comptime && p.ty == type_sym {
                    // For comptime TYPE parameters (comptime T: type), the type is `type`
                    Ok(Type::COMPTIME_TYPE)
                } else if self.type_mentions_type_param(p.ty, &type_param_names)
                    || self.type_mentions_comptime_value_param(p.ty, &value_param_names)
                {
                    // This parameter's type is a type parameter (`x: T`) or a
                    // composite mentioning one (`a: [T; 3]`, `p: ptr const T`;
                    // RUE-172), or an array whose length names a comptime value
                    // parameter (`arr: [i32; N]`, RUE-16). Use ComptimeType as a
                    // placeholder - the actual type is determined at
                    // specialization.
                    //
                    // The deferred type still needs declaration-time validation
                    // for array lengths that do *not* depend on specialization:
                    // `[T; A]` with an undefined `A` should be E0481 here, not
                    // an ICE when the function is later instantiated (RUE-381).
                    self.validate_deferred_signature_type_lengths(
                        p.ty,
                        &type_param_names,
                        &value_param_names,
                        &value_param_type_syms,
                        span,
                    )?;
                    Ok(Type::COMPTIME_TYPE)
                } else {
                    // Regular params OR comptime VALUE params (comptime n: i32)
                    self.resolve_type(p.ty, span)
                }
            })
            .collect::<CompileResult<Vec<_>>>()?;
        let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

        // For generic functions, we can't resolve the return type yet if it references
        // a type parameter - either directly (`-> T`) or inside a composite
        // (`-> [T; 3]`, RUE-172).
        let ret_type = if self.type_mentions_type_param(return_type_sym, &type_param_names)
            || self.type_mentions_comptime_value_param(return_type_sym, &value_param_names)
        {
            // Return type references a type parameter (`-> T`, `-> [T; 3]`) or
            // an array length naming a comptime value parameter (`-> [i32; N]`,
            // RUE-16) - use placeholder, resolved at specialization.
            self.validate_deferred_signature_type_lengths(
                return_type_sym,
                &type_param_names,
                &value_param_names,
                &value_param_type_syms,
                span,
            )?;
            Type::COMPTIME_TYPE
        } else {
            // A slice type `[T]` is second-class (ADR-0037, ADR-0043,
            // RUE-322): it is a fat-pointer view valid only in argument
            // position and may not be returned (E0487).
            self.reject_slice_escape(return_type_sym, span, ErrorKind::SliceReturnNotAllowed)?;
            self.resolve_type(return_type_sym, span)?
        };

        // Foreign `extern "C"` declarations are gated behind the `c_ffi` preview
        // and their signature types enforced (ADR-0064 P1 + Amendment 1). See
        // `check_extern_signature_type` for the diagnostic layering.
        if is_extern {
            self.require_preview(
                PreviewFeature::CFfi,
                "an `extern \"C\"` foreign declaration",
                span,
            )?;
            for &param_type in &param_types {
                self.check_extern_signature_type(param_type, span)?;
            }
            if ret_type != Type::UNIT {
                self.check_extern_signature_type(ret_type, span)?;
            }
        }

        // Rue-to-C exports (`pub extern "C" fn`, ADR-0064 P4) share the FFI
        // signature-type contract with imports, and add the P4 export-thunk's
        // narrower scope: the callee thunk marshals only integer/pointer scalars
        // that fit the argument-register budget shared by both targets, so an
        // aggregate parameter/return, a by-reference or generic parameter, or a
        // parameter list wider than that budget is rejected rather than
        // silently mis-marshaled. The C name must also not collide with the
        // program entry point.
        if is_c_export {
            self.require_preview(PreviewFeature::CFfi, "a `pub extern \"C\" fn` export", span)?;
            let reject = |reason: String| -> CompileResult<()> {
                Err(CompileError::new(
                    ErrorKind::ExportSignatureUnsupported {
                        name: name_str.to_string(),
                        reason,
                    },
                    span,
                ))
            };
            if name_str == "main" {
                reject(
                    "an export named `main` collides with the program entry point; \
                     give it a different C name"
                        .to_string(),
                )?;
            }
            if is_generic {
                reject(
                    "a generic function has no single C symbol; export a concrete \
                     (non-`comptime`) function"
                        .to_string(),
                )?;
            }
            // Shared FFI-safety type gate (rejects floats, enums, by-value
            // arrays, and non-`@repr(c)` aggregates with their own diagnostics).
            for &param_type in &param_types {
                self.check_extern_signature_type(param_type, span)?;
            }
            if ret_type != Type::UNIT {
                self.check_extern_signature_type(ret_type, span)?;
            }
            // P4 thunk scope: integer/pointer scalars only, within the smaller
            // (SysV, 6) register budget so one export is valid on both targets.
            const P4_EXPORT_REGISTER_BUDGET: usize = 6;
            for (index, mode) in param_modes.iter().enumerate() {
                if *mode != RirParamMode::Normal {
                    reject(format!(
                        "parameter {} uses a by-reference (`borrow`/`inout`) mode, which \
                         does not cross a C boundary; pass a raw pointer instead",
                        index + 1
                    ))?;
                }
            }
            for &param_type in &param_types {
                if Self::is_aggregate_type(param_type) {
                    reject(format!(
                        "aggregate parameter `{}` is not supported by the P4 export thunk \
                         (register repacking of `@repr(c)` aggregates across the export \
                         boundary is future work); pass a pointer to the aggregate instead",
                        self.format_type_name(param_type)
                    ))?;
                }
            }
            if ret_type != Type::UNIT && Self::is_aggregate_type(ret_type) {
                reject(format!(
                    "aggregate return `{}` is not supported by the P4 export thunk; \
                     return a scalar or write through an out-pointer parameter instead",
                    self.format_type_name(ret_type)
                ))?;
            }
            if param_types.len() > P4_EXPORT_REGISTER_BUDGET {
                reject(format!(
                    "{} scalar parameters exceed the {}-register argument budget the P4 \
                     export thunk supports; reduce the parameter count",
                    param_types.len(),
                    P4_EXPORT_REGISTER_BUDGET
                ))?;
            }
        }

        // Allocate parameter data in the arena
        let params_range = self.param_arena.alloc(
            param_names.into_iter(),
            param_types.into_iter(),
            param_modes.into_iter(),
            param_comptime.into_iter(),
        );

        let internal_name = self.internal_function_name(name, span.file_id);
        let info = FunctionInfo {
            params: params_range,
            return_type: ret_type,
            return_type_sym,
            body,
            declaration,
            span,
            is_generic,
            is_pub,
            is_unchecked,
            is_extern,
            is_c_export,
            allow_unused_function,
            allow_unused_variable,
            allow_unreachable_code,
            file_id: span.file_id,
        };
        self.functions_by_file_name
            .insert((span.file_id, name), internal_name);
        self.function_source_names.insert(internal_name, name);

        self.functions.insert(internal_name, info);
        Ok(())
    }

    /// Collect methods defined inline in a struct.
    fn collect_struct_methods(
        &mut self,
        type_name: Spur,
        methods: &rue_rir::RirStructMethodsRange,
        span: Span,
    ) -> CompileResult<()> {
        let struct_id = match self.structs_by_file_name.get(&(span.file_id, type_name)) {
            Some(id) => *id,
            None => {
                let type_name_str = self.interner.resolve(&type_name).to_string();
                return Err(CompileError::new(
                    ErrorKind::UnknownType(type_name_str),
                    span,
                ));
            }
        };
        let struct_type = Type::new_struct(struct_id);

        let methods = self.rir.struct_methods(methods);
        for method_ref in methods {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params,
                return_type,
                body,
                has_self,
                self_mode,
                self_is_mut,
                returns_borrow,
                ..
            } = &method_inst.data
            {
                let owner_name = self.interner.resolve(&type_name).to_string();
                self.declaration_type_observer = (!owner_name.starts_with("__anon_struct_"))
                    .then_some((
                        method_inst.span.file_id,
                        self.interner.resolve(method_name).to_string(),
                        Some(owner_name),
                        if *has_self {
                            super::DeclarationTypeDependencySourceKind::Method
                        } else {
                            super::DeclarationTypeDependencySourceKind::AssociatedFunction
                        },
                        super::DeclarationTypeDependencyKind::Signature,
                    ));
                // Use StructId in key to support anonymous struct methods
                let key = (struct_id, *method_name);
                if self.methods.contains_key(&key) {
                    let type_name_str = self.interner.resolve(&type_name).to_string();
                    let method_name_str = self.interner.resolve(&*method_name).to_string();
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: type_name_str,
                            method_name: method_name_str,
                        },
                        method_inst.span,
                    ));
                }

                let params = self.rir.params(params);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_modes: Vec<RirParamMode> = params.iter().map(|p| p.mode).collect();
                let param_comptime: Vec<bool> = params.iter().map(|p| p.is_comptime).collect();

                // Place-returning borrow accessors (ADR-0062, RUE-662): the
                // `-> borrow T` declaration shape is checked before any type
                // in the signature resolves.
                check_accessor_declaration_shape(
                    self.rir,
                    self.interner,
                    method_ref,
                    Some(*body),
                    true,
                    &self.preview_features,
                )?;
                // `Self` in a method signature (parameter or return position)
                // resolves to the enclosing struct's type, just like the
                // receiver does. Named-struct inline methods reach this path;
                // anonymous-struct methods resolve Self elsewhere (RUE-123).
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.resolve_type_with_self(p.ty, struct_type, method_inst.span))
                    .collect::<CompileResult<Vec<_>>>()?;
                let ret_type =
                    self.resolve_type_with_self(*return_type, struct_type, method_inst.span)?;

                // Allocate method parameters in the arena
                let param_range = self.param_arena.alloc_method(
                    param_names,
                    param_types,
                    param_modes,
                    param_comptime,
                );

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        self_mode: *self_mode,
                        self_is_mut: *self_is_mut,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                        returns_borrow: *returns_borrow,
                    },
                );
                self.named_method_declarations.insert(key, method_ref);
            }
        }
        Ok(())
    }

    /// Collect a constant declaration by its `(file, name)` key.
    ///
    /// Constants are compile-time values. They come in two kinds, decided by
    /// what the initializer evaluates to:
    ///
    /// - **Module bindings** — the initializer is an `@import(...)`, an alias
    ///   of another module binding (`const m = other;`), or a member-access
    ///   chain ending at a re-export (`const math = std.math;`). These are
    ///   **per-file scoped** (ADR-0026: every file writes its own imports),
    ///   stored as tagged resolutions keyed by the declaring file — two files
    ///   binding the same name, even to different modules, is fine (RUE-113).
    /// - **Value constants** — everything else: the initializer is evaluated
    ///   through the comptime engine (`sema::comptime_eval`), so negated
    ///   literals, arithmetic, and any other comptime-evaluable expression
    ///   are legal initializers (RUE-171). Value constants are stored by
    ///   defining file and name; same-file duplicates are still E0418, while
    ///   distinct modules may each define the same source name and expose it
    ///   through module-qualified access.
    ///
    /// Collection is dependency-driven: an initializer that references another
    /// not-yet-collected constant resolves that indexed declaration first, so
    /// declaration order does not matter. Cyclic initializers are E0461.
    fn collect_const_by_key(&mut self, key: (FileId, Spur)) -> CompileResult<()> {
        debug_assert!(self.declaration_binding_active);
        if self.const_resolutions.contains_key(&key) {
            return Ok(());
        }
        if self.const_resolution_in_progress.contains(&key) {
            // Re-entering a key that is mid-evaluation: the initializers
            // form a cycle. Report it (never loop on it).
            let pos = self
                .const_resolution_in_progress
                .iter()
                .position(|k| k == &key)
                .expect("key was just found in in_progress");
            let cycle = self.const_resolution_in_progress[pos..]
                .iter()
                .chain(std::iter::once(&key))
                .map(|(_, n)| self.interner.resolve(n))
                .collect::<Vec<_>>()
                .join(" -> ");
            let span = self
                .indexed_const(key)
                .expect("in-progress constant must have an indexed declaration")
                .span;
            return Err(CompileError::new(
                ErrorKind::ConstInitializerCycle { cycle },
                span,
            ));
        }
        let Some(p) = self.indexed_const(key) else {
            return Ok(());
        };
        let (file_id, name) = key;
        let name_str = self.interner.resolve(&name).to_string();
        let previous_dependency_source = self
            .named_const_dependency_source
            .replace((file_id, name_str.clone()));

        // A value-constant name that collides with a function, struct, or enum
        // is a top-level name collision (E0436), but that decision is made
        // order-independently after all declarations are collected, in
        // `check_const_cross_kind_collisions` — not here, where collection is
        // dependency-ordered and the conflicting item may not be registered
        // yet (RUE-239).

        // The declared integer type (when the annotation names one) becomes
        // the type context for arithmetic in the initializer, so intermediate
        // results are checked at the operand type just like a `comptime { }`
        // block (RUE-230). A non-integer or unresolvable annotation yields
        // `None`; the annotation error, if any, resurfaces in
        // `const_type_for_value` where it did before.
        let declared_ty = p
            .ty_sym
            .and_then(|sym| self.resolve_type(sym, p.span).ok())
            .filter(Type::is_integer);

        self.const_resolution_in_progress.push(key);
        let outcome = self.eval_const_initializer(p.init, file_id, declared_ty);
        self.named_const_dependency_source = previous_dependency_source;
        self.const_resolution_in_progress.pop();
        let outcome = outcome?;

        match outcome {
            ConstInit::Module(module_ty) => {
                if let Some(ty_sym) = p.ty_sym {
                    // No type annotation can name a module type, so an
                    // annotated module binding is always a mismatch.
                    let declared = self.resolve_type(ty_sym, p.span)?;
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: declared.safe_name_with_pool(Some(&self.type_pool)),
                            found: module_ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        p.span,
                    ));
                }
                self.const_resolutions.insert(
                    (file_id, name),
                    super::ConstResolution::ModuleBinding(ConstInfo {
                        is_pub: p.is_pub,
                        ty: module_ty,
                        value: ConstValue::Type(module_ty),
                        span: p.span,
                    }),
                );
            }
            ConstInit::Value(value) => {
                let ty = self.const_type_for_value(value, p.ty_sym, p.init, p.span, &name_str)?;
                let info = ConstInfo {
                    is_pub: p.is_pub,
                    ty,
                    value,
                    span: p.span,
                };
                self.const_resolutions
                    .insert(key, super::ConstResolution::Value(info));
            }
        }
        Ok(())
    }

    /// During declaration binding, resolve an indexed constant on demand.
    /// After the `BoundSema` boundary, the namespace is closed and a missing
    /// entry is an authoritative lookup miss.
    ///
    /// A collection failure propagates instead of degrading to a lookup miss.
    /// Every on-demand probe sits on a path that hard-errors after a miss
    /// (E0204/E0707/E0481), and struct fields resolve before the main const
    /// walk in `resolve_remaining_declarations` — so a swallowed initializer
    /// error (e.g. E0705 from a failed `@import`) was *replaced* by the
    /// downstream miss diagnostic rather than re-reported later (RUE-724).
    /// A name with no same-file const declaration still resolves to `Ok(None)`.
    pub(crate) fn resolve_indexed_const_binding_impl(
        &mut self,
        name: Spur,
        file_id: FileId,
    ) -> CompileResult<Option<ConstValue>> {
        debug_assert!(self.declaration_binding_active);
        self.ensure_const_collected(name, file_id)?;
        Ok(self
            .resolve_const_info_in_file(name, file_id)
            .map(|info| info.value))
    }

    fn indexed_const(&self, key: (FileId, Spur)) -> Option<PendingConst> {
        let declaration = *self
            .bound_const_candidates
            .as_ref()
            .expect("const candidate authority must be installed before declaration resolution")
            .candidates(key.0, key.1)
            .first()?;
        let inst = self.rir.get(declaration);
        let InstData::ConstDecl {
            is_pub, ty, init, ..
        } = inst.data
        else {
            unreachable!("constant declaration index contains only ConstDecl instructions")
        };
        Some(PendingConst {
            is_pub,
            ty_sym: ty,
            init,
            span: inst.span,
        })
    }

    fn validate_indexed_const_declarations(&self) -> CompileResult<()> {
        let mut seen = HashSet::new();
        for &declaration in self
            .bound_const_candidates
            .as_ref()
            .expect("const candidate authority must be installed before declaration resolution")
            .all_candidates()
        {
            let inst = self.rir.get(declaration);
            let InstData::ConstDecl { name, .. } = inst.data else {
                unreachable!("constant declaration index contains only ConstDecl instructions")
            };
            if !seen.insert((inst.span.file_id, name)) {
                return Err(CompileError::new(
                    crate::declaration_validation::duplicate_constant(self.interner.resolve(&name)),
                    inst.span,
                ));
            }
        }
        Ok(())
    }

    /// Collect the same-file constant declaration named at a declaration-time
    /// reference site. Cross-module constants resolve through module members.
    fn ensure_const_collected(
        &mut self,
        name: Spur,
        referencing_file: FileId,
    ) -> CompileResult<()> {
        let same_file_key = (referencing_file, name);
        if !self
            .bound_const_candidates
            .as_ref()
            .expect("const candidate authority must be installed before declaration resolution")
            .candidates(referencing_file, name)
            .is_empty()
        {
            self.collect_const_by_key(same_file_key)?;
        }
        Ok(())
    }

    /// Evaluate a constant initializer at compile time.
    ///
    /// Module-flavored forms (`@import(...)`, aliases of module bindings,
    /// member-access chains ending at a module) are resolved here; everything
    /// else is delegated to the comptime engine via
    /// [`Self::eval_const_value_expr`].
    fn eval_const_initializer(
        &mut self,
        init: InstRef,
        file_id: FileId,
        declared_ty: Option<Type>,
    ) -> CompileResult<ConstInit> {
        let init_inst = self.rir.get(init);
        let span = init_inst.span;

        match &init_inst.data {
            // @import("path") evaluates to a module at compile time.
            InstData::Intrinsic { name, args } => {
                if *name == self.known.import {
                    assert_eq!(
                        self.rir.intrinsic_args(args).len(),
                        1,
                        "compiler import preflight rejects malformed @import calls"
                    );
                    let arg_refs = self.rir.intrinsic_args(args);
                    let arg_inst = self.rir.get(arg_refs.get(0).unwrap());
                    let import_path = match &arg_inst.data {
                        InstData::StringConst {
                            content: path_spur, ..
                        } => self.interner.resolve(path_spur).to_string(),
                        _ => unreachable!("compiler import preflight requires a string literal"),
                    };

                    let module_id = self.resolve_canonical_import(&import_path, span)?;

                    Ok(ConstInit::Module(Type::new_module(module_id)))
                } else {
                    // Other intrinsics are not supported in const context.
                    let intrinsic_name = self.interner.resolve(name).to_string();
                    Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: format!("@{} intrinsic", intrinsic_name),
                        },
                        span,
                    ))
                }
            }

            // A name: another module binding (alias) or a value constant.
            InstData::VarRef { name, .. } => {
                let name = *name;
                self.ensure_const_collected(name, file_id)?;
                if let Some(binding) = self.module_binding(&(file_id, name)).cloned() {
                    self.record_named_const_dependency(
                        super::NamedConstDependencyTargetEvent::ModuleBinding {
                            file: file_id.index(),
                            name: self.interner.resolve(&name).to_string(),
                        },
                    );
                    // `const m2 = m;` — aliasing a module binding declared in
                    // this file yields the same module (RUE-160).
                    return Ok(ConstInit::Module(binding.ty));
                }
                if let Some(info) = self.resolve_const_info_in_file(name, file_id).cloned() {
                    self.record_named_const_dependency(
                        super::NamedConstDependencyTargetEvent::ValueConst {
                            file: info.span.file_id.index(),
                            name: self.interner.resolve(&name).to_string(),
                        },
                    );
                    // Apply the uniform privacy rule; the initializer's own
                    // span identifies the referencing file (E0460, RUE-183).
                    self.check_unqualified_visibility(
                        "constant",
                        self.interner.resolve(&name),
                        info.span.file_id,
                        info.is_pub,
                        span,
                    )?;
                    return Ok(ConstInit::Value(info.value));
                }
                if let Some((fn_file_id, is_pub)) =
                    self.collect_free_function_signature_during_binding(name, Some(file_id))?
                {
                    self.record_named_const_dependency(
                        super::NamedConstDependencyTargetEvent::FreeFunction {
                            file: fn_file_id.index(),
                            name: self.interner.resolve(&name).to_string(),
                        },
                    );
                    let function_key = self
                        .resolve_function_name_local(name, file_id)
                        .unwrap_or_else(|| self.internal_function_name(name, fn_file_id));
                    let fn_name = self.interner.resolve(&name).to_string();
                    self.check_unqualified_visibility(
                        "function", &fn_name, fn_file_id, is_pub, span,
                    )?;
                    return Ok(ConstInit::Value(ConstValue::Function(function_key)));
                }
                // Not a constant: let the comptime engine decide (it rejects
                // unknown names and type names as non-evaluable).
                self.eval_const_value_expr(init, file_id, span, declared_ty)
            }

            // Member access: `base.member` where `base` is a module —
            // `const math = std.math;` (alias of a re-export, RUE-160) or
            // `const X: i32 = m.ANSWER;` (a member value constant).
            InstData::FieldGet { base, field } => {
                let (base, field) = (*base, *field);
                match self.eval_const_initializer(base, file_id, None)? {
                    ConstInit::Module(module_ty) => {
                        let module_id = module_ty
                            .as_module()
                            .expect("ConstInit::Module holds a module type");
                        self.resolve_module_member_const(module_id, field, file_id, span)
                    }
                    ConstInit::Value(_) => Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: "member access on a non-module value".to_string(),
                        },
                        span,
                    )),
                }
            }

            // Module-member call: `const OptI = std.option.Option(i64);`.
            // In expression syntax this parses as a method call on the module
            // receiver. In const context we only reduce it when the target is a
            // module function returning `type` with compile-time-known args;
            // ordinary runtime module function calls remain non-const.
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                let (receiver, method) = (*receiver, *method);
                match self.eval_const_initializer(receiver, file_id, None)? {
                    ConstInit::Module(module_ty) => {
                        let module_id = module_ty
                            .as_module()
                            .expect("ConstInit::Module holds a module type");
                        self.eval_module_member_comptime_call(
                            module_id, method, args, file_id, span,
                        )
                    }
                    ConstInit::Value(_) => Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: "method call on a non-module value".to_string(),
                        },
                        span,
                    )),
                }
            }

            // A string literal is directly a constant value (RUE-957). It is
            // stored as the interned content; use sites materialize it like
            // an inline literal (local string table -> `.rodata` `str`).
            InstData::StringConst { content, .. } => {
                Ok(ConstInit::Value(ConstValue::String(*content)))
            }

            // Everything else: literals, arithmetic, comptime blocks, ... —
            // evaluated by the comptime engine.
            _ => self.eval_const_value_expr(init, file_id, span, declared_ty),
        }
    }

    /// Evaluate a (non-module) constant initializer through the comptime
    /// engine (`sema::comptime_eval`).
    ///
    /// A file-level const has no enclosing function, so there is no HM
    /// `resolved_types` map. Instead we infer one up front from the declared
    /// integer type (`infer_const_init_types`) and hand it to the engine, so
    /// arithmetic is checked at the operand type — matching the `comptime { }`
    /// block path (operand-type mismatch → E0206, operand/intermediate
    /// overflow → E1200, RUE-230). The final value is still range-checked
    /// against the declared type in [`Self::const_type_for_value`] (which also
    /// rejects a missing annotation, E0475).
    fn eval_const_value_expr(
        &mut self,
        init: InstRef,
        file_id: FileId,
        span: Span,
        declared_ty: Option<Type>,
    ) -> CompileResult<ConstInit> {
        // Pre-collect referenced constants: the engine's file-level-constant
        // lookup only consults the finished `constants` table, so anything
        // this initializer names must be collected first. (Also required
        // before type inference so referenced constants' types are known.)
        self.ensure_const_init_deps_collected(init, file_id)?;

        // Pre-resolve module-member accesses (`m.CONST`) appearing as
        // operands. The engine can't resolve them itself (no file/collector
        // context), so it reads their values from this map keyed by the
        // `FieldGet` instruction (RUE-267). Runs before type inference so the
        // members' constants are collected for `infer_const_init_types`.
        let mut const_module_members: HashMap<InstRef, ConstValue> = HashMap::new();
        self.collect_const_module_members(init, file_id, &mut const_module_members)?;

        // Infer operand types for the initializer expression so the engine
        // checks arithmetic at the operand type instead of the raw-i64
        // fallback (RUE-230). Reports E0206 on a mismatched-operand-type
        // binary op before evaluation, mirroring the runtime path.
        let mut resolved_types: HashMap<InstRef, Type> = HashMap::new();
        self.infer_const_init_types(init, file_id, declared_ty, &mut resolved_types)?;

        let canonical_identity = match self.named_const_dependency_source.as_ref() {
            Some((file, name)) => {
                // Query-owned const shells are candidates until initializer
                // evaluation classifies ValueConst versus ModuleBinding. Use
                // that exact candidate channel before consulting the final
                // stable universe: a same-named function or type is a source
                // collision, not evidence that the const has the wrong kind.
                let producer = match self.epoch_local_const_candidate_producer(*file, name) {
                    Ok(producer) => producer.into_comptime_producer(),
                    Err(crate::SemanticBodyExportFailure::MissingStableIdentity) => self
                        .canonical_definition_producer(
                            *file,
                            name,
                            None,
                            crate::StableDefinitionKind::ValueConst,
                        )
                        .map_err(|failure| {
                            CompileError::new(
                                ErrorKind::InternalError(format!(
                                    "failed to issue canonical const producer: {failure:?}"
                                )),
                                span,
                            )
                        })?,
                    Err(failure) => {
                        return Err(CompileError::new(
                            ErrorKind::InternalError(format!(
                                "failed to issue epoch-local const candidate producer: {failure:?}"
                            )),
                            span,
                        ));
                    }
                };
                Some((producer, crate::CanonicalArguments::default()))
            }
            None => None,
        };
        let mut env = super::comptime_eval::ComptimeEnv::for_const_init(
            &resolved_types,
            &const_module_members,
            file_id,
            canonical_identity,
        );
        match self.eval_const_expr(init, &mut env)? {
            Some(value) => Ok(ConstInit::Value(value)),
            None => Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: "this expression".to_string(),
                },
                span,
            )),
        }
    }

    /// Infer integer operand types for a const initializer's expression tree,
    /// recording each integer-typed node in `map`, so the comptime engine can
    /// apply the same operand-type checks a `comptime { }` block gets from HM
    /// inference (RUE-230). Two effects:
    ///
    /// - **E1200 (operand/intermediate overflow):** every arithmetic node is
    ///   typed, so `finish_arith` range-checks each intermediate at the
    ///   operand type (not just the final value against the declared type).
    /// - **E0206 (mixed operand types):** a binary op whose operands carry
    ///   different concrete integer types is rejected here, before evaluation,
    ///   mirroring the runtime diagnostic (`expected <lhs>, found <rhs>`, span
    ///   on the rhs operand).
    ///
    /// `expected` is the type context flowing down from the declared const
    /// type: integer literals adopt it, while references to other constants
    /// carry their own declared type. Returns the node's integer type when
    /// known (`None` for non-integer or unconstrained nodes).
    fn infer_const_init_types(
        &mut self,
        expr: InstRef,
        file_id: FileId,
        expected: Option<Type>,
        map: &mut HashMap<InstRef, Type>,
    ) -> CompileResult<Option<Type>> {
        let expected = expected.filter(Type::is_integer);
        let ty = match &self.rir.get(expr).data {
            // A literal adopts the type context (the declared const type, or a
            // surrounding operand's type propagated down).
            InstData::IntConst(_) => expected,

            // A reference to another constant carries that constant's declared
            // type, regardless of the surrounding context.
            InstData::VarRef { name, .. } => self
                .resolve_const_info_in_file(*name, file_id)
                .map(|info| info.ty)
                .filter(|t| t.is_integer()),

            // A module-member access (`m.CONST`) carries the member constant's
            // declared type, so arithmetic against it is checked at that width
            // (RUE-267). Resolve the member in the RECEIVER MODULE's file
            // (RUE-572): same-named constants across modules may differ in
            // width, so lookup is authoritative in the receiver module.
            InstData::FieldGet { base, field } => {
                let (base, field) = (*base, *field);
                let via_module = if let InstData::VarRef { name, .. } = &self.rir.get(base).data {
                    self.module_binding(&(file_id, *name))
                        .and_then(|info| info.ty.as_module())
                        .map(|module_id| self.module_registry.get_def(module_id).file_id)
                        .and_then(|module_file| {
                            self.value_const(&(module_file, field)).map(|info| info.ty)
                        })
                } else {
                    None
                };
                via_module.filter(|t| t.is_integer())
            }

            // Unary ops preserve the operand's (integer) type. A bare integer
            // literal operand is left untyped, though: typing `-5` as `u8`
            // would make the engine reject it as `E0801 cannot negate u8`
            // before the value/annotation range check runs, whereas a negative
            // literal const is diagnosed as out-of-range against its declared
            // type in `const_type_for_value` (spec 6.5:5). Only the facets that
            // need operand types — binary arithmetic — are threaded here.
            InstData::Neg { operand } | InstData::BitNot { operand } => {
                let operand = *operand;
                if matches!(self.rir.get(operand).data, InstData::IntConst(_)) {
                    None
                } else {
                    self.infer_const_init_types(operand, file_id, expected, map)?
                }
            }

            // Logical NOT is a bool; still walk the operand for nested checks.
            InstData::Not { operand } => {
                let operand = *operand;
                self.infer_const_init_types(operand, file_id, None, map)?;
                None
            }

            // Arithmetic/bitwise binary ops: operands must share a type. The
            // result type is that shared type (so `finish_arith` checks at the
            // operand width). A mismatch of two known concrete types is E0206.
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs }
            | InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                let lt = self.infer_const_init_types(lhs, file_id, expected, map)?;
                let rt = self.infer_const_init_types(rhs, file_id, expected, map)?;
                if let (Some(l), Some(r)) = (lt, rt) {
                    if l != r {
                        return Err(CompileError::new(
                            ErrorKind::TypeMismatch {
                                expected: l.safe_name_with_pool(Some(&self.type_pool)),
                                found: r.safe_name_with_pool(Some(&self.type_pool)),
                            },
                            self.rir.get(rhs).span,
                        ));
                    }
                }
                lt.or(rt).or(expected)
            }

            // Shifts: the result follows the lhs; the shift amount is an
            // independent operand (spec 4.3a:10).
            InstData::Shl { lhs, rhs } | InstData::Shr { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                let lt = self.infer_const_init_types(lhs, file_id, expected, map)?;
                self.infer_const_init_types(rhs, file_id, None, map)?;
                lt
            }

            // Comparisons/logical: bool result. Operands anchor each other so
            // a bare literal compared against a typed const is checked at that
            // const's type; the node itself is not integer-typed.
            InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                let lt = self.infer_const_init_types(lhs, file_id, None, map)?;
                let rt = self.infer_const_init_types(rhs, file_id, None, map)?;
                if lt.is_some() && rt.is_none() {
                    self.infer_const_init_types(rhs, file_id, lt, map)?;
                } else if rt.is_some() && lt.is_none() {
                    self.infer_const_init_types(lhs, file_id, rt, map)?;
                }
                None
            }

            InstData::And { lhs, rhs } | InstData::Or { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.infer_const_init_types(lhs, file_id, None, map)?;
                self.infer_const_init_types(rhs, file_id, None, map)?;
                None
            }

            // `comptime { expr }` forwards the context to its inner expression.
            InstData::Comptime { expr: inner } => {
                let inner = *inner;
                self.infer_const_init_types(inner, file_id, expected, map)?
            }

            // A block's tail expression carries the context; `let` initializers
            // are typed by their own value (no annotation context available
            // here), and non-tail statements carry no expected type.
            InstData::Block { instructions } => {
                let stmt_refs: Vec<InstRef> = self.rir.block_insts(instructions).values().collect();
                let n = stmt_refs.len();
                let mut tail_ty = None;
                for (i, &stmt_ref) in stmt_refs.iter().enumerate() {
                    let is_tail = i + 1 == n;
                    let stmt_expected = if is_tail { expected } else { None };
                    let t = if let InstData::Alloc { init, .. } = &self.rir.get(stmt_ref).data {
                        let init = *init;
                        self.infer_const_init_types(init, file_id, None, map)?;
                        None
                    } else {
                        self.infer_const_init_types(stmt_ref, file_id, stmt_expected, map)?
                    };
                    if is_tail {
                        tail_ty = t;
                    }
                }
                tail_ty
            }

            _ => None,
        };
        if let Some(t) = ty {
            map.insert(expr, t);
        }
        Ok(ty)
    }

    /// Walk a constant initializer expression and collect (on demand) every
    /// constant it references, so declaration order does not decide whether
    /// dependent constants are available.
    ///
    /// Only the expression forms the engine can evaluate are walked; anything
    /// else makes the expression non-evaluable anyway. A block-local `let`
    /// that shadows a constant name still triggers collection of the constant
    /// — harmless, since every constant is collected eventually.
    fn ensure_const_init_deps_collected(
        &mut self,
        expr: InstRef,
        file_id: FileId,
    ) -> CompileResult<()> {
        match &self.rir.get(expr).data {
            InstData::VarRef { name, .. } => {
                let name = *name;
                self.ensure_const_collected(name, file_id)
            }
            InstData::Neg { operand }
            | InstData::Not { operand }
            | InstData::BitNot { operand } => {
                let operand = *operand;
                self.ensure_const_init_deps_collected(operand, file_id)
            }
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs }
            | InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs }
            | InstData::And { lhs, rhs }
            | InstData::Or { lhs, rhs }
            | InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.ensure_const_init_deps_collected(lhs, file_id)?;
                self.ensure_const_init_deps_collected(rhs, file_id)
            }
            InstData::Comptime { expr: inner } => {
                let inner = *inner;
                self.ensure_const_init_deps_collected(inner, file_id)
            }
            InstData::Block { instructions } => {
                let stmt_refs: Vec<InstRef> = self.rir.block_insts(instructions).values().collect();
                for stmt_ref in stmt_refs {
                    self.ensure_const_init_deps_collected(stmt_ref, file_id)?;
                }
                Ok(())
            }
            InstData::Alloc { init, .. } => {
                let init = *init;
                self.ensure_const_init_deps_collected(init, file_id)
            }
            _ => Ok(()),
        }
    }

    /// Walk a constant initializer and resolve every module-member access
    /// (`m.CONST`) appearing in it to its compile-time value, keyed by the
    /// `FieldGet` instruction. The comptime engine has no file or
    /// constant-collector context, so it can only evaluate a module member as
    /// a whole initializer (via [`Self::eval_const_initializer`]'s `FieldGet`
    /// arm); this pre-pass lets one appear as an operand of a larger
    /// expression too — `const A: i32 = 1 + m.ANSWER;` (RUE-267).
    ///
    /// Only a member whose base resolves to a module and whose value is a
    /// scalar constant is recorded. A non-module base is left untouched (the
    /// engine reports it), while a privacy violation propagates as its usual
    /// error (E0706). The tree walk mirrors [`Self::ensure_const_init_deps_collected`].
    fn collect_const_module_members(
        &mut self,
        expr: InstRef,
        file_id: FileId,
        out: &mut HashMap<InstRef, ConstValue>,
    ) -> CompileResult<()> {
        match &self.rir.get(expr).data {
            InstData::FieldGet { base, field } => {
                let (base, field) = (*base, *field);
                // Probe the base; only a module base has a member value here.
                // A non-module base (or an unresolvable one) is left for the
                // engine to reject, matching the pre-fix behavior.
                if let Ok(ConstInit::Module(module_ty)) =
                    self.eval_const_initializer(base, file_id, None)
                {
                    let module_id = module_ty
                        .as_module()
                        .expect("ConstInit::Module holds a module type");
                    let span = self.rir.get(expr).span;
                    // Privacy errors (E0706) propagate; a re-export member
                    // yields a module, not a value, so it stays unrecorded.
                    if let ConstInit::Value(value) =
                        self.resolve_module_member_const(module_id, field, file_id, span)?
                    {
                        out.insert(expr, value);
                    }
                }
                Ok(())
            }
            InstData::Neg { operand }
            | InstData::Not { operand }
            | InstData::BitNot { operand } => {
                let operand = *operand;
                self.collect_const_module_members(operand, file_id, out)
            }
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs }
            | InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs }
            | InstData::And { lhs, rhs }
            | InstData::Or { lhs, rhs }
            | InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.collect_const_module_members(lhs, file_id, out)?;
                self.collect_const_module_members(rhs, file_id, out)
            }
            InstData::Comptime { expr: inner } => {
                let inner = *inner;
                self.collect_const_module_members(inner, file_id, out)
            }
            InstData::Block { instructions } => {
                let stmt_refs: Vec<InstRef> = self.rir.block_insts(instructions).values().collect();
                for stmt_ref in stmt_refs {
                    self.collect_const_module_members(stmt_ref, file_id, out)?;
                }
                Ok(())
            }
            InstData::Alloc { init, .. } => {
                let init = *init;
                self.collect_const_module_members(init, file_id, out)
            }
            _ => Ok(()),
        }
    }

    /// Resolve `module.member` in const context, where `member` must be a
    /// constant declared in the module's file: a module binding (re-export)
    /// yields its module, a value constant yields its value. Visibility
    /// follows the usual rule (10.3/10.4): `pub` is visible everywhere,
    /// non-`pub` only from the module's own directory (E0706).
    fn resolve_module_member_const(
        &mut self,
        module_id: crate::types::ModuleId,
        member: Spur,
        accessing_file: FileId,
        span: Span,
    ) -> CompileResult<ConstInit> {
        let module_def = self.module_registry.get_def(module_id);
        let import_path = module_def.import_path.clone();
        let member_str = self.interner.resolve(&member).to_string();

        // The compiler bridge joins each durable module identity to this
        // request's FileId before semantic analysis begins.
        let module_file_id = Some(module_def.file_id);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or(module_def.file_path);

        // Collect the member's declaration on demand (the module's file may
        // appear later in the declaration walk).
        if let Some(mfile) = module_file_id {
            self.collect_const_by_key((mfile, member))?;
        }

        // A module-binding member (re-export or alias) yields its module.
        if let Some(mfile) = module_file_id {
            if let Some(info) = self.module_binding(&(mfile, member)) {
                let (is_pub, ty) = (info.is_pub, info.ty);
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                self.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::ModuleBinding {
                        file: mfile.index(),
                        name: member_str.clone(),
                    },
                );
                return Ok(ConstInit::Module(ty));
            }
        }

        // A value constant declared in the module's file yields its value.
        if let Some(mfile) = module_file_id {
            if let Some(info) = self.value_const(&(mfile, member)) {
                let (is_pub, value) = (info.is_pub, info.value);
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                self.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::ValueConst {
                        file: mfile.index(),
                        name: member_str.clone(),
                    },
                );
                return Ok(ConstInit::Value(value));
            }
        }

        // A function member yields a callable alias. It is intentionally not a
        // first-class runtime value: call analysis rewrites calls through this
        // ConstValue to the real callee, and expression materialization rejects
        // it.
        if let Some(mfile) = module_file_id {
            if let Some((_fn_file_id, is_pub)) = self.find_free_function_decl(member, Some(mfile)) {
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                self.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::FreeFunction {
                        file: mfile.index(),
                        name: member_str.clone(),
                    },
                );
                let function_key = self.internal_function_name(member, mfile);
                return Ok(ConstInit::Value(ConstValue::Function(function_key)));
            }
        }

        // A struct/enum member is a TYPE value (`m.Row`): usable anywhere a
        // type-valued constant is — most importantly as a type argument of a
        // generic instantiation in a const initializer
        // (`const Rows = std.arraybuf.ArrayBuf(records.Row);`, RUE-630).
        // The old behavior rejected this as "a type value ... not supported",
        // a claim stale since type-valued constants landed.
        if let Some(mfile) = module_file_id {
            if let Some(struct_id) = self.structs_by_file_name.get(&(mfile, member)).copied() {
                let def = self
                    .type_pool
                    .struct_metadata(struct_id)
                    .expect("named struct must have declaration metadata");
                let is_pub = def.is_pub;
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                self.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::NamedType {
                        file: mfile.index(),
                        name: member_str.clone(),
                        kind: super::DeclarationTypeDependencyTargetKind::Struct,
                    },
                );
                return Ok(ConstInit::Value(ConstValue::Type(Type::new_struct(
                    struct_id,
                ))));
            }
            if let Some(enum_id) = self.enums_by_file_name.get(&(mfile, member)).copied() {
                let def = self
                    .type_pool
                    .enum_metadata(enum_id)
                    .expect("named enum must have declaration metadata");
                let is_pub = def.is_pub;
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                self.record_named_const_dependency(
                    super::NamedConstDependencyTargetEvent::NamedType {
                        file: mfile.index(),
                        name: member_str.clone(),
                        kind: super::DeclarationTypeDependencyTargetKind::Enum,
                    },
                );
                return Ok(ConstInit::Value(ConstValue::Type(Type::new_enum(enum_id))));
            }
        }

        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: import_path,
                member_name: member_str,
            },
            span,
        ))
    }

    /// Evaluate `module.member(args...)` in const context when `member` is a
    /// module function returning `type`. This is the module-scope analogue of
    /// function-local `let O = std.option.Option(i64);`: the alias is a
    /// compile-time type value, not a runtime call.
    fn eval_module_member_comptime_call(
        &mut self,
        module_id: crate::types::ModuleId,
        member: Spur,
        args: &rue_rir::RirCallArgsRange,
        accessing_file: FileId,
        span: Span,
    ) -> CompileResult<ConstInit> {
        let module_def = self.module_registry.get_def(module_id);
        let import_path = module_def.import_path.clone();
        let member_str = self.interner.resolve(&member).to_string();
        let module_file_id = Some(module_def.file_id);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or(module_def.file_path);

        let Some(mfile) = module_file_id else {
            return Err(CompileError::new(
                ErrorKind::UnknownModuleMember {
                    module_name: import_path,
                    member_name: member_str,
                },
                span,
            ));
        };
        let Some((_fn_file_id, is_pub)) =
            self.collect_free_function_signature_during_binding(member, Some(mfile))?
        else {
            return Err(CompileError::new(
                ErrorKind::UnknownModuleMember {
                    module_name: import_path,
                    member_name: member_str,
                },
                span,
            ));
        };
        self.check_const_member_visibility(
            is_pub,
            &member_str,
            &module_file_path,
            accessing_file,
            span,
        )?;

        let function_key = self
            .resolve_function_name_local(member, mfile)
            .unwrap_or_else(|| self.internal_function_name(member, mfile));
        let Some(fn_info) = self.functions.get(&function_key).copied() else {
            return Err(CompileError::new(
                ErrorKind::UnknownModuleMember {
                    module_name: import_path,
                    member_name: member_str,
                },
                span,
            ));
        };
        self.record_named_const_dependency(super::NamedConstDependencyTargetEvent::FreeFunction {
            file: fn_info.file_id.index(),
            name: member_str.clone(),
        });
        if fn_info.return_type != Type::COMPTIME_TYPE {
            return Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("call to non-type function `{member_str}`"),
                },
                span,
            ));
        }

        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_modes = self.param_arena.modes(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let args = self.rir.call_args(args);
        if args.len() != param_names.len() {
            return Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("call to `{member_str}`"),
                },
                span,
            ));
        }
        // Module-scope const collection reduces this call before ordinary
        // function-body analysis can see it. Preserve the same exact source
        // mode contract as every runtime call (RUE-634).
        self.validate_explicit_call_modes(&args, param_modes.iter().copied())?;

        let all_params_comptime = !param_names.is_empty() && param_comptime.iter().all(|&c| c);
        let eligible = param_names.is_empty() || all_params_comptime;
        if !eligible {
            return Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: format!("call to `{member_str}`"),
                },
                span,
            ));
        }

        let mut callee_types: HashMap<Spur, Type> = HashMap::new();
        let mut callee_values: HashMap<Spur, ConstValue> = HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            self.ensure_const_init_deps_collected(arg.value, accessing_file)?;
            let mut const_module_members: HashMap<InstRef, ConstValue> = HashMap::new();
            self.collect_const_module_members(
                arg.value,
                accessing_file,
                &mut const_module_members,
            )?;
            let resolved_types: HashMap<InstRef, Type> = HashMap::new();
            let mut env = super::comptime_eval::ComptimeEnv::for_const_init(
                &resolved_types,
                &const_module_members,
                accessing_file,
                None,
            );
            let Some(value) = self.eval_const_expr(arg.value, &mut env)? else {
                return Err(CompileError::new(
                    ErrorKind::ConstExprNotSupported {
                        expr_kind: format!("argument to `{member_str}`"),
                    },
                    self.rir.get(arg.value).span,
                ));
            };
            match (param_comptime_type[i], value) {
                (true, ConstValue::Type(ty)) => {
                    callee_types.insert(param_names[i], ty);
                }
                (true, ConstValue::Unit) => {
                    callee_types.insert(param_names[i], Type::UNIT);
                }
                (true, _) => {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "argument for '{}' must be a type",
                                self.interner.resolve(&param_names[i])
                            ),
                        },
                        self.rir.get(arg.value).span,
                    ));
                }
                (false, value) => {
                    callee_values.insert(param_names[i], value);
                }
            }
        }

        match self
            .reduce_type_ctor_body(function_key, &callee_types, &callee_values, span)
            .map_err(|e| Self::label_ctor_instantiation_site(e, span))?
        {
            Some(ConstValue::Type(ty)) => Ok(ConstInit::Value(ConstValue::Type(ty))),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "the type constructor '{}' did not reduce to a concrete type \
                         at compile time",
                        member_str
                    ),
                },
                span,
            )),
        }
    }

    /// Find a free function declaration in RIR, optionally restricted to one
    /// source file. This is used during const collection, which is
    /// dependency-ordered and can run before the main function-signature pass
    /// has populated `self.functions`.
    fn find_free_function_decl(
        &self,
        target: Spur,
        file_id: Option<FileId>,
    ) -> Option<(FileId, bool)> {
        let inst_ref = self
            .declaration_index
            .first_free_function(target, file_id)?;
        let inst = self.rir.get(inst_ref);
        let InstData::FnDecl { is_pub, .. } = &inst.data else {
            unreachable!("free-function index contains only FnDecl instructions");
        };
        Some((inst.span.file_id, *is_pub))
    }

    /// Materialize a free function's signature during declaration binding.
    ///
    /// This supports dependency-ordered const, aggregate, and signature type
    /// resolution before the main declaration walk reaches the callee. It is
    /// deliberately unavailable as a body-phase fallback: after `BoundSema`,
    /// source function lookup is read-only and a miss is authoritative.
    ///
    /// These declaration-time callers include const collection,
    /// struct-field/enum-payload type resolution, and signature-type
    /// resolution, all of which can run before the main declaration walk
    /// reaches the callee's `FnDecl` (RUE-603).
    pub(crate) fn collect_free_function_signature_binding_impl(
        &mut self,
        target: Spur,
        file_id: Option<FileId>,
    ) -> CompileResult<Option<(FileId, bool)>> {
        debug_assert!(
            self.declaration_binding_active,
            "source function signatures may only be materialized during declaration binding"
        );
        let Some(inst_ref) = self.declaration_index.first_free_function(target, file_id) else {
            return Ok(None);
        };
        let (span, is_pub, return_type, body, is_unchecked, is_extern, is_c_export) = {
            let inst = self.rir.get(inst_ref);
            let InstData::FnDecl {
                is_pub,
                return_type,
                body,
                is_unchecked,
                is_extern,
                is_c_export,
                ..
            } = &inst.data
            else {
                unreachable!("free-function index contains only FnDecl instructions");
            };
            (
                inst.span,
                *is_pub,
                *return_type,
                *body,
                *is_unchecked,
                *is_extern,
                *is_c_export,
            )
        };

        let internal_target = self.internal_function_name(target, span.file_id);
        if !self.functions.contains_key(&internal_target) {
            // Cycle guard (see `fn_signatures_in_flight`): collecting this
            // signature resolves its parameter/return types, which may collect
            // other signatures on demand; a signature cycle must terminate as
            // "not found" (the referencing site reports E0204) instead of
            // recursing forever.
            if !self.fn_signatures_in_flight.insert(internal_target) {
                return Ok(None);
            }
            let collected = self.collect_function_signature(
                inst_ref,
                target,
                return_type,
                body,
                span,
                is_pub,
                is_unchecked,
                is_extern,
                is_c_export,
            );
            self.fn_signatures_in_flight.remove(&internal_target);
            collected?;
        }
        Ok(Some((span.file_id, is_pub)))
    }

    /// The visibility rule for module members accessed in const context
    /// (10.3/10.4): `pub` members are visible from anywhere; non-`pub`
    /// members only from the defining module's own directory (E0706).
    pub(crate) fn check_const_member_visibility(
        &self,
        is_pub: bool,
        member_str: &str,
        module_file_path: &str,
        accessing_file: FileId,
        span: Span,
    ) -> CompileResult<()> {
        if is_pub {
            return Ok(());
        }
        let same_dir = match self.get_file_path(accessing_file) {
            Some(accessing) => {
                std::path::Path::new(accessing).parent()
                    == std::path::Path::new(module_file_path).parent()
            }
            // Be permissive if we can't determine the path (unit tests).
            None => true,
        };
        if same_dir {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "const".to_string(),
                    name: member_str.to_string(),
                },
                span,
            ))
        }
    }

    /// Determine a value constant's type from its evaluated value and
    /// annotation.
    ///
    /// A value constant **requires** a type annotation (spec 6.5:4, RUE-179);
    /// an unannotated one is E0475, with a help suggesting the annotation
    /// that would have been inferred (smallest of `i32`/`i64`/`u64` for
    /// integers, `bool`/`()` otherwise). Only module bindings — which never
    /// take this path — are exempt. An annotated integer constant adopts any
    /// integer annotation its value fits in (RUE-161); a value out of range
    /// of the annotation is rejected at the declaration (E0800).
    fn const_type_for_value(
        &mut self,
        value: ConstValue,
        ty_sym: Option<Spur>,
        init: InstRef,
        span: Span,
        name: &str,
    ) -> CompileResult<Type> {
        use super::comptime_eval::const_int_fits;

        let inferred = match value {
            ConstValue::Integer(v) => {
                if const_int_fits(v, Type::I32) {
                    Type::I32
                } else if const_int_fits(v, Type::I64) {
                    Type::I64
                } else {
                    Type::U64
                }
            }
            ConstValue::Bool(_) => Type::BOOL,
            ConstValue::Unit => Type::UNIT,
            // A string constant is always the static view type `str`; the
            // owning (`StrBuf`) and fixed (`Str(N)`) representations stay
            // runtime-only (RUE-957).
            ConstValue::String(_) => self.get_or_create_str_struct(span)?,
            ConstValue::Function(_) | ConstValue::Type(_) => Type::COMPTIME_TYPE,
        };

        let Some(ty_sym) = ty_sym else {
            if matches!(value, ConstValue::Function(_)) {
                return Ok(inferred);
            }
            // Type aliases are compile-time-only bindings, like function
            // aliases: they do not require a runtime value annotation.
            if matches!(value, ConstValue::Type(_)) {
                return Ok(inferred);
            }
            return Err(CompileError::new(
                ErrorKind::ConstMissingTypeAnnotation {
                    name: name.to_string(),
                },
                span,
            )
            .with_help(format!(
                "add a type annotation: `const {}: {} = ...;`",
                name,
                inferred.safe_name_with_pool(Some(&self.type_pool))
            )));
        };
        // The annotation resolves like any signature type (unknown names are
        // E0204) and the value is validated against it.
        // A slice type `[T]` is second-class (ADR-0037, ADR-0043, RUE-322): a
        // `const` is a top-level binding, never an argument, so it cannot name
        // a slice (E0489).
        self.reject_slice_escape(ty_sym, span, ErrorKind::SliceEscapesScope)?;
        let declared = self.resolve_type(ty_sym, span)?;

        // An integer value adopts any integer annotation it fits in.
        if let ConstValue::Integer(v) = value {
            if declared.is_integer() {
                if const_int_fits(v, declared) {
                    return Ok(declared);
                }
                let init_span = self.rir.get(init).span;
                let ty_name = declared.safe_name_with_pool(Some(&self.type_pool));
                return Err(if v >= 0 {
                    CompileError::new(
                        ErrorKind::LiteralOutOfRange {
                            value: v as u64,
                            ty: ty_name,
                        },
                        init_span,
                    )
                } else {
                    // LiteralOutOfRange's payload is unsigned; negative
                    // values mirror the comptime-block diagnostic instead.
                    CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!("value {} is out of range for type {}", v, ty_name),
                        },
                        init_span,
                    )
                });
            }
        }

        if self.types_equivalent(declared, inferred) {
            return Ok(declared);
        }
        Err(CompileError::new(
            ErrorKind::TypeMismatch {
                expected: declared.safe_name_with_pool(Some(&self.type_pool)),
                found: inferred.safe_name_with_pool(Some(&self.type_pool)),
            },
            span,
        ))
    }
}

/// The declaration legality of a `-> borrow T` accessor (ADR-0062): the result
/// position requires the `borrow_accessors` preview (6.6:3), the receiver is a
/// shared `borrow self` (6.6:4), every other parameter is a plain by-value
/// guard input (6.6:5), the body's only exit is its trailing `yield` (6.6:6),
/// and that `yield` hands out a receiver-rooted place (6.6:7).
///
/// These are legality rules on the declaration, so a declared-but-uncalled
/// accessor is exactly as ill-formed as a called one. Every producer runs them
/// over every accessor declaration it admits, which is what keeps the rules
/// independent of the driver's on-demand body analysis. What this function
/// owns is the *RIR walk*; which forms are illegal and how each one reads is
/// [`crate::declaration_validation`]'s, shared with the driver's reparsed-AST
/// producer. The body rules are syntactic over the RIR, so they belong here
/// with the signature rules; the single part that is not — whether a
/// method-call link in the yielded chain names an accessor — is documented on
/// [`accessor_yield_root`] and stays with the demanded path.
///
/// The declaring `FnDecl` is the only carrier of `returns_borrow`: the durable
/// signature records the result type's source spelling, which never contains
/// the result-position `borrow` qualifier.
///
/// The preview gate runs first, so an ungated program reports E1100 rather
/// than a shape error about a form it cannot name yet.
pub(super) fn check_accessor_declaration_shape(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    declaration: InstRef,
    body: Option<InstRef>,
    has_named_owner: bool,
    preview_features: &rue_error::PreviewFeatures,
) -> CompileResult<()> {
    let inst = rir.get(declaration);
    let InstData::FnDecl {
        params,
        has_self,
        self_mode,
        returns_borrow: true,
        ..
    } = &inst.data
    else {
        return Ok(());
    };
    let span = inst.span;
    super::require_preview_feature(
        preview_features,
        crate::declaration_validation::ACCESSOR_PREVIEW_FEATURE,
        crate::declaration_validation::ACCESSOR_PREVIEW_SUBJECT,
        span,
    )?;
    // An accessor hands out a projection of its receiver, so the receiver is
    // the first thing that has to exist and be a shared borrow. `self` is
    // carried by `has_self`, so the parameter list is exactly the guard
    // inputs.
    let receiver = if !has_named_owner {
        AccessorReceiverForm::FreeFunction
    } else if !*has_self {
        AccessorReceiverForm::AssociatedFunction
    } else {
        match self_mode {
            RirParamMode::Borrow => AccessorReceiverForm::BorrowSelf,
            RirParamMode::Inout => AccessorReceiverForm::InoutSelf,
            RirParamMode::Normal => AccessorReceiverForm::ValueSelf,
        }
    };
    let params = rir.params(params);
    if let Some(violation) = crate::declaration_validation::accessor_signature(
        receiver,
        params
            .iter()
            .map(|param| accessor_parameter_form(param.mode, param.is_comptime)),
    ) {
        use crate::declaration_validation::AccessorSignatureViolation as Violation;
        return Err(match violation {
            Violation::Receiver { kind, note } => {
                let error = CompileError::new(kind, span);
                match note {
                    Some(note) => error.with_note(note),
                    None => error,
                }
            }
            Violation::Parameter { kind, ordinal } => {
                let span = params
                    .iter()
                    .nth(ordinal)
                    .map_or(span, |parameter| parameter.span);
                CompileError::new(kind, span)
            }
        });
    }
    if let Some(body) = body {
        let (verdict, span) = accessor_body_verdict(rir, interner, body);
        if let Some(kind) = crate::declaration_validation::accessor_body_error(&verdict) {
            return Err(CompileError::new(kind, span));
        }
    }
    Ok(())
}

/// 6.6:14 at the declaration seam (RUE-1282): reject an accessor cycle for
/// every accessor the program declares, whether or not anything calls one.
///
/// A cycle has no finite expansion — a property of the declarations alone —
/// so this runs beside the other accessor shape rules rather than waiting for
/// a call site to demand a body. The edge set is the `self`-receiver accessor
/// calls in each body: a call `self.m()` where `m` is an accessor of the same
/// owner resolves to exactly that accessor, so the edge is certain without
/// resolving a single type. A re-entrant call through any other receiver (a
/// `borrow`-mode guard parameter of the owner's own type) is not decidable
/// here and stays with the demanded-path checks: the analysis-time identity
/// check for a direct cycle and the CFG dependency graph for a mutual one.
pub(super) fn check_accessor_acyclicity(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    pending: &[super::binding_manifest::PendingDeclarationPayload],
) -> CompileResult<()> {
    struct DeclaredAccessor {
        name: Arc<str>,
        body: InstRef,
        span: Span,
        source_order: u32,
    }
    // Group declared accessors by owning nominal. `module_path` joins the key
    // so same-named owners from different modules never share a graph.
    let mut groups: std::collections::BTreeMap<(Arc<str>, Arc<str>), Vec<DeclaredAccessor>> =
        std::collections::BTreeMap::new();
    for payload in pending {
        let super::binding_manifest::DeclarationPayloadSource::Callable { body } = payload.source
        else {
            continue;
        };
        let Some(owner) = payload.shell.identity.owner.clone() else {
            continue;
        };
        let InstData::FnDecl {
            returns_borrow: true,
            has_self: true,
            ..
        } = rir.get(payload.declaration).data
        else {
            continue;
        };
        groups
            .entry((payload.shell.identity.module_path.clone(), owner))
            .or_default()
            .push(DeclaredAccessor {
                name: payload.shell.identity.name.clone(),
                body,
                span: payload.shell.declaration_span,
                source_order: payload.shell.source_order,
            });
    }

    let Some(self_sym) = interner.get("self") else {
        return Ok(());
    };
    for accessors in groups.values_mut() {
        // Source order makes the reported accessor the first of the cycle's
        // members the program declares.
        accessors.sort_by_key(|accessor| accessor.source_order);
        let methods: Vec<crate::declaration_validation::AccessorOwnerMethod> = accessors
            .iter()
            .map(
                |accessor| crate::declaration_validation::AccessorOwnerMethod {
                    name: accessor.name.clone(),
                    is_accessor: true,
                    self_call_targets: rir_self_call_targets(
                        rir,
                        interner,
                        self_sym,
                        accessor.body,
                    ),
                },
            )
            .collect();
        for accessor in accessors.iter() {
            if crate::declaration_validation::accessor_self_call_cycle(&accessor.name, &methods) {
                return Err(CompileError::new(
                    crate::declaration_validation::accessor_recursion_error(&accessor.name),
                    accessor.span,
                )
                .with_note(crate::declaration_validation::ACCESSOR_RECURSION_NOTE));
            }
        }
    }
    Ok(())
}

/// The method names one accessor body calls on its own `self` receiver, in the
/// normalized (sorted, deduplicated) form [`accessor_self_call_cycle`]'s edges
/// use.
///
/// [`accessor_self_call_cycle`]: crate::declaration_validation::accessor_self_call_cycle
fn rir_self_call_targets(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    self_sym: Spur,
    body: InstRef,
) -> Arc<[Arc<str>]> {
    let mut targets: Vec<Arc<str>> = body_instructions(rir, body)
        .into_iter()
        .filter_map(|inst_ref| {
            let InstData::MethodCall {
                receiver, method, ..
            } = &rir.get(inst_ref).data
            else {
                return None;
            };
            let receiver_is_self = matches!(
                &rir.get(*receiver).data,
                InstData::VarRef { name, .. } if *name == self_sym
            );
            receiver_is_self.then(|| Arc::from(interner.resolve(method)))
        })
        .collect();
    targets.sort();
    targets.dedup();
    Arc::from(targets)
}

/// The 6.6:5 form of one RIR parameter.
pub(super) fn accessor_parameter_form(
    mode: RirParamMode,
    is_comptime: bool,
) -> AccessorParameterForm {
    if is_comptime {
        return AccessorParameterForm::Comptime;
    }
    match mode {
        RirParamMode::Normal => AccessorParameterForm::ByValue,
        RirParamMode::Borrow => AccessorParameterForm::Borrow,
        RirParamMode::Inout => AccessorParameterForm::Inout,
    }
}

/// Decide 6.6:6 and 6.6:7 for one accessor body over the RIR, with the span of
/// the offending form.
///
/// The rules apply in the spec's own order — a body with no trailing `yield`
/// is reported as that, not as whatever its last expression yields — and each
/// verdict is turned into a diagnostic by
/// [`crate::declaration_validation::accessor_body_error`], so this producer
/// and the driver's reparsed-AST producer cannot disagree on wording.
fn accessor_body_verdict(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    body: InstRef,
) -> (AccessorBodyVerdict, Span) {
    let Some(trailing) = trailing_yield(rir, body) else {
        return (
            AccessorBodyVerdict::MissingTrailingYield,
            rir.get(body).span,
        );
    };
    if let Some((exit, span)) = accessor_body_exit(rir, body, trailing) {
        return (AccessorBodyVerdict::OtherExit(exit), span);
    }
    let InstData::Yield(operand) = rir.get(trailing).data else {
        unreachable!("the trailing exit is a `yield` by construction")
    };
    match accessor_yield_root(rir, interner, operand) {
        Some((root, span)) => (AccessorBodyVerdict::YieldNotReceiverRooted(root), span),
        None => (AccessorBodyVerdict::WellFormed, rir.get(body).span),
    }
}

/// Every instruction lexically contained in `body`, `body` itself included,
/// in lowering (source) order.
///
/// The walk stops at a nested declaration — a nested `fn`, drop function,
/// struct, or anonymous type owns its own body, and predeclaration reaches
/// that declaration on its own entry — so a containment question answered
/// here is answered about this body alone.
fn body_instructions(rir: &rue_rir::Rir, body: InstRef) -> Vec<InstRef> {
    let mut pending = vec![body];
    let mut seen = HashSet::new();
    let mut contained = Vec::new();
    let mut children = Vec::new();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        contained.push(current);
        if current != body
            && matches!(
                rir.get(current).data,
                InstData::FnDecl { .. }
                    | InstData::DropFnDecl { .. }
                    | InstData::StructDecl { .. }
                    | InstData::EnumDecl { .. }
                    | InstData::AnonStructType { .. }
                    | InstData::AnonEnumType { .. }
            )
        {
            continue;
        }
        children.clear();
        rir.child_instructions(current, &mut children);
        pending.extend(children.iter().copied());
    }
    // Instruction indices are assigned as astgen lowers, so ordering by index
    // reports the first offending form in the source rather than an arbitrary
    // one from the traversal.
    contained.sort_unstable_by_key(|inst_ref| inst_ref.as_u32());
    contained
}

/// The rest of 6.6:6: an accessor body's trailing `yield` is its *only* exit —
/// no second `yield`, no `return`, no `?` (E0254).
///
/// "Contains" is the spec's own wording, and containment is decidable from the
/// RIR, so this runs at the declaration for every accessor the program
/// declares rather than only for a body some call site demands. The
/// comptime-pruned branch of an `if` counts: 6.6:6 forbids the form appearing
/// in the body, not merely on a path the specialization keeps.
fn accessor_body_exit(
    rir: &rue_rir::Rir,
    body: InstRef,
    trailing: InstRef,
) -> Option<(AccessorExitForm, Span)> {
    for inst_ref in body_instructions(rir, body) {
        let inst = rir.get(inst_ref);
        let exit = match &inst.data {
            InstData::Yield(_) if inst_ref != trailing => AccessorExitForm::SecondYield,
            InstData::Ret(_) => AccessorExitForm::Return,
            InstData::Try { .. } => AccessorExitForm::Try,
            _ => continue,
        };
        return Some((exit, inst.span));
    }
    None
}

/// 6.6:7 over the RIR: what the trailing `yield`'s operand chain is rooted at,
/// and the span of the form that decided it (E0255).
///
/// Everything this rule needs is syntactic except one link. A nested
/// method-call link is legal only when the callee is *itself* an accessor,
/// and which method a call names is a resolved-type question, so this walk
/// accepts any method call and keeps descending to the chain's root — it never
/// reports [`AccessorYieldRootForm::PlainMethod`], which only a producer with
/// the callee in hand may claim. That leaves exactly one residual for a
/// declaration nothing calls: a chain through a plain (non-accessor) method of
/// the receiver, such as `yield self.plain();`, whose rejection stays with
/// [`super::control_flow`]'s demanded-path check. Every other shape — a local,
/// a non-receiver parameter, a computed value, a chain rooted at anything but
/// `self` — is decided here with no call site.
fn accessor_yield_root(
    rir: &rue_rir::Rir,
    interner: &lasso::ThreadedRodeo,
    operand: InstRef,
) -> Option<(AccessorYieldRootForm, Span)> {
    let self_sym = interner.get_or_intern("self");
    let mut current = operand;
    loop {
        let inst = rir.get(current);
        let span = inst.span;
        match &inst.data {
            InstData::VarRef { name, .. } => {
                if *name == self_sym {
                    return None;
                }
                let root = AccessorYieldRootForm::Named(Arc::from(interner.resolve(name)));
                return Some((root, span));
            }
            InstData::FieldGet { base, .. } | InstData::IndexGet { base, .. } => current = *base,
            InstData::MethodCall { receiver, .. } => current = *receiver,
            _ => return Some((AccessorYieldRootForm::Value, span)),
        }
    }
}

/// The single trailing `yield` that an accessor body falls through to
/// (6.6:6, ADR-0062 phase 1), if it has one.
///
/// Which instruction is the trailing exit is decidable from the RIR alone, so
/// the declaration seam decides a body with no trailing `yield` for every
/// accessor in the program. [`accessor_body_exit`] then finds the *other*
/// exits from the same seam.
fn trailing_yield(rir: &rue_rir::Rir, body: InstRef) -> Option<InstRef> {
    // A single-statement body lowers to the instruction itself; a
    // multi-statement body lowers to a block whose last instruction is the
    // trailing exit.
    let trailing = match &rir.get(body).data {
        InstData::Block { instructions } => rir.block_insts(instructions).values().last(),
        _ => Some(body),
    };
    trailing.filter(|inst_ref| matches!(rir.get(*inst_ref).data, InstData::Yield(_)))
}

/// [`trailing_yield`] as the body engine demands it: the trailing exit it
/// records for the body it is about to analyze, or E0254.
pub(super) fn accessor_trailing_yield(rir: &rue_rir::Rir, body: InstRef) -> CompileResult<InstRef> {
    trailing_yield(rir, body).ok_or_else(|| {
        CompileError::new(
            crate::declaration_validation::accessor_missing_yield_error(),
            rir.get(body).span,
        )
    })
}

/// A constant declaration projected from the canonical RIR declaration index.
#[derive(Clone, Copy)]
struct PendingConst {
    is_pub: bool,
    ty_sym: Option<Spur>,
    init: InstRef,
    span: Span,
}

/// What a constant initializer evaluated to.
enum ConstInit {
    /// A module: an `@import(...)`, an alias of a module binding, or a
    /// member-access chain ending at a re-export. Becomes a per-file module
    /// binding. The `Type` is always a `Type::Module`.
    Module(Type),
    /// A compile-time value: becomes a global value constant.
    Value(ConstValue),
}
