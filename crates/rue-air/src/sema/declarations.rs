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

use lasso::{Key, Spur};
use rue_builtins::is_reserved_type_name;
use rue_error::{
    CompileError, CompileResult, CopyStructNonCopyFieldError, ErrorKind, PreviewFeature, ice,
};
use rue_rir::{InstData, InstRef, RirDirective, RirParamMode};
use rue_span::{FileId, Span};

use super::{ConstInfo, ConstValue, FunctionInfo, InferenceContext, MethodInfo, Sema};
use crate::inference::{FunctionSig, MethodSig};
use crate::types::{EnumDef, StructDef, StructField, StructId, Type};

impl<'a> Sema<'a> {
    /// Build an `InferenceContext` from the collected type information.
    ///
    /// This should be called after the collection phase and builds the
    /// pre-computed maps needed for Hindley-Milner type inference.
    /// Building this once and reusing for all function analyses avoids
    /// the O(n²) cost of rebuilding these maps per function.
    ///
    /// # Performance
    ///
    /// This converts all function/method signatures to use `InferType`
    /// (which handles arrays structurally rather than by ID). This conversion
    /// is done once instead of per-function.
    pub fn build_inference_context(&self) -> InferenceContext {
        // Build function signatures with InferType for constraint generation
        let func_sigs: HashMap<Spur, FunctionSig> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSig {
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                        is_generic: info.is_generic,
                        param_modes: self.param_arena.modes(info.params).to_vec(),
                        param_comptime: self.param_arena.comptime(info.params).to_vec(),
                        param_names: self.param_arena.names(info.params).to_vec(),
                        param_type_syms: self
                            .rir
                            .get_params(info.rir_params_start, info.rir_params_len)
                            .iter()
                            .map(|p| p.ty)
                            .collect(),
                        return_type_sym: info.return_type_sym,
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::new_struct(id))
        let struct_types: HashMap<Spur, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::new_struct(*id)))
            .collect();

        // Build enum types map (name -> Type::new_enum(id))
        let enum_types: HashMap<Spur, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::new_enum(*id)))
            .collect();

        // Build method signatures with InferType for constraint generation
        let mut method_sigs: HashMap<(StructId, Spur), MethodSig> = self
            .methods
            .iter()
            .map(|((struct_id, method_name), info)| {
                (
                    (*struct_id, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        // Register builtin-type method signatures (String::len, etc.) so inference
        // can resolve their return types. Without this, a builtin method-call result
        // used in a binop (e.g. `s.len() == 2`) was left unconstrained and resolved
        // to `<error>`, poisoning the other operand's literal-range check. (RUE-95)
        self.register_builtin_method_sigs(&mut method_sigs);

        // Constant types (resolved during declaration gathering) so a const
        // reference in a function body infers to its declared type instead of
        // `<error>` (RUE-142).
        let const_types: HashMap<Spur, Type> = self
            .constants
            .iter()
            .map(|(name, info)| (*name, info.ty))
            .collect();

        // Integer constant values, so an array length naming a `const`
        // (`[i32; K]`) resolves to a concrete length during inference (RUE-16).
        let const_values: HashMap<Spur, i128> = self
            .constants
            .iter()
            .filter_map(|(name, info)| info.value.as_int_value().map(|v| (*name, v)))
            .collect();

        // Module-binding types (`const utils = @import(...)`), keyed by the
        // declaring file: bindings are per-file scoped (RUE-113), so a
        // reference resolves against the file it appears in.
        let module_binding_types: HashMap<(FileId, Spur), Type> = self
            .module_bindings
            .iter()
            .map(|(key, info)| (*key, info.ty))
            .collect();

        InferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
            const_types,
            const_values,
            module_binding_types,
        }
    }
    /// Check if a directive list contains the @copy directive
    pub(crate) fn has_copy_directive(&self, directives: &[RirDirective]) -> bool {
        let copy_sym = self.interner.get("copy");
        for directive in directives {
            if Some(directive.name) == copy_sym {
                return true;
            }
        }
        false
    }

    /// Phase 0: Order-independent name-collision check across the function and
    /// type (struct/enum) name spaces (spec 10.3:1, 10.5:1, RUE-239).
    ///
    /// Functions, structs, enums, and constants are all top-level items sharing
    /// **one** name space, so any two of them with the same name collide —
    /// regardless of their kinds, their order within a file, or the
    /// command-line order of the files they come from. This pass over the
    /// merged RIR is the order-independent source of truth for collisions among
    /// **functions, structs, and enums**; the previously order-dependent gap
    /// was a function colliding with a struct/enum, which was never checked.
    /// The per-kind checks in [`register_type_names`](Self::register_type_names)
    /// and the compiler's cross-file duplicate scan remain as backstops but no
    /// longer decide legality by order.
    ///
    /// Constants are handled separately, after collection, in
    /// [`check_const_cross_kind_collisions`](Self::check_const_cross_kind_collisions):
    /// their global-vs-per-file identity (value constant vs `@import` module
    /// binding, spec 10.4:8) is only known once initializers are evaluated, so
    /// they cannot be classified from the raw RIR here.
    ///
    /// The error code reuses the existing per-kind codes based on the kinds
    /// involved, so pre-existing diagnostics are unchanged:
    /// - two types (struct/enum) → E0405 (`DuplicateTypeDefinition`)
    /// - any pair involving a function → E0436 (`DuplicateFunctionDefinition`)
    ///
    /// Methods and associated functions live in a type-scoped namespace, not
    /// the global one, so they are excluded here (matching the collection
    /// logic in `resolve_remaining_declarations`).
    pub(crate) fn check_top_level_name_collisions(&self) -> CompileResult<()> {
        // Gather method / associated-function inst refs to exclude: they are
        // namespaced under their enclosing type, not the global name space.
        let mut method_refs: HashSet<InstRef> = HashSet::new();
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::AnonStructType {
                    methods_start,
                    methods_len,
                    ..
                }
                | InstData::StructDecl {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    for r in self.rir.get_inst_refs(*methods_start, *methods_len) {
                        method_refs.insert(r);
                    }
                }
                _ => {}
            }
        }

        // For each name, whether the first item seen was a type (struct/enum);
        // the alternative is a function.
        let mut seen: HashMap<Spur, (bool, Span)> = HashMap::new();
        for (inst_ref, inst) in self.rir.iter() {
            let (name, is_type) = match &inst.data {
                InstData::StructDecl { name, .. } | InstData::EnumDecl { name, .. } => {
                    (*name, true)
                }
                InstData::FnDecl { name, has_self, .. } => {
                    if *has_self || method_refs.contains(&inst_ref) {
                        continue;
                    }
                    (*name, false)
                }
                _ => continue,
            };

            match seen.get(&name).copied() {
                None => {
                    seen.insert(name, (is_type, inst.span));
                }
                Some((first_is_type, first_span)) => {
                    let name_str = self.interner.resolve(&name).to_string();
                    // Two types collide as a duplicate type (E0405); any pair
                    // involving a function is a duplicate function (E0436).
                    let err_kind = if first_is_type && is_type {
                        ErrorKind::DuplicateTypeDefinition {
                            type_name: name_str,
                        }
                    } else {
                        ErrorKind::DuplicateFunctionDefinition {
                            function_name: name_str,
                        }
                    };
                    return Err(CompileError::new(err_kind, inst.span)
                        .with_label("first defined here".to_string(), first_span));
                }
            }
        }
        Ok(())
    }

    /// Post-collection check: a value constant's name must not collide with a
    /// function, struct, or enum (spec 10.3:1, 10.5:1, RUE-239).
    ///
    /// Runs after [`resolve_declarations`](Self::resolve_declarations), when
    /// [`Sema::constants`] holds exactly the value constants — module bindings
    /// went to [`Sema::module_bindings`], which are per-file scoped and exempt
    /// (spec 10.4:8). Comparing the fully-populated tables makes the check
    /// order-independent: a `const shared` and a `fn shared` collide whichever
    /// file or definition comes first. All such collisions reuse E0436, so a
    /// value-constant-vs-function collision no longer depends on which was
    /// collected first (previously E0418 only when the function came first).
    pub(crate) fn check_const_cross_kind_collisions(&self) -> CompileResult<()> {
        for (name, info) in self.constants.iter() {
            if self.functions.contains_key(name)
                || self.structs.contains_key(name)
                || self.enums.contains_key(name)
            {
                let name_str = self.interner.resolve(name).to_string();
                return Err(CompileError::new(
                    ErrorKind::DuplicateFunctionDefinition {
                        function_name: name_str,
                    },
                    info.span,
                ));
            }
        }
        Ok(())
    }

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
                    variants_start,
                    variants_len,
                    payloads_len,
                    ..
                } => {
                    // Tuple-variant payloads (RUE-221, ADR-0038) are gated
                    // behind the `enum_payloads` preview feature. A payload
                    // region of length 0 means every variant is
                    // discriminant-only (C-like), which is always allowed.
                    if *payloads_len > 0 {
                        self.require_preview(
                            PreviewFeature::EnumPayloads,
                            "enum payloads (variants that carry data)",
                            inst.span,
                        )?;
                    }

                    let enum_name = self.interner.resolve(&*name).to_string();

                    // Check for collision with built-in type names
                    if is_reserved_type_name(&enum_name) {
                        return Err(CompileError::new(
                            ErrorKind::ReservedTypeName {
                                type_name: enum_name,
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate type definitions (struct or enum with same name)
                    if self.enums.contains_key(name) || self.structs.contains_key(name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: enum_name,
                            },
                            inst.span,
                        ));
                    }

                    let variants = self.rir.get_symbols(*variants_start, *variants_len);

                    // Check for duplicate variant names
                    let mut seen_variants: HashSet<Spur> = HashSet::new();
                    for variant_name in &variants {
                        if !seen_variants.insert(*variant_name) {
                            let variant_name_str =
                                self.interner.resolve(&*variant_name).to_string();
                            return Err(CompileError::new(
                                ErrorKind::DuplicateVariant {
                                    enum_name: enum_name.clone(),
                                    variant_name: variant_name_str,
                                },
                                inst.span,
                            ));
                        }
                    }

                    // Convert variant symbols to strings
                    let variant_names: Vec<String> = variants
                        .iter()
                        .map(|v| self.interner.resolve(&*v).to_string())
                        .collect();

                    let enum_def = EnumDef {
                        name: enum_name,
                        variants: variant_names,
                        // Payload types are resolved in phase 2
                        // (`resolve_enum_payloads`), once all type names are
                        // registered, so payloads may reference any struct/enum.
                        variant_payloads: Vec::new(),
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based EnumId
                    let (enum_id, _) = self.type_pool.register_enum(*name, enum_def);

                    // Register in enum lookup with pool-based EnumId
                    self.enums.insert(*name, enum_id);
                }
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    is_pub,
                    is_linear,
                    name,
                    ..
                } => {
                    let struct_name = self.interner.resolve(&*name).to_string();

                    // Check for collision with built-in type names
                    if is_reserved_type_name(&struct_name) {
                        return Err(CompileError::new(
                            ErrorKind::ReservedTypeName {
                                type_name: struct_name,
                            },
                            inst.span,
                        ));
                    }

                    // Check for duplicate type definitions (struct or enum with same name)
                    if self.structs.contains_key(name) || self.enums.contains_key(name) {
                        return Err(CompileError::new(
                            ErrorKind::DuplicateTypeDefinition {
                                type_name: struct_name,
                            },
                            inst.span,
                        ));
                    }

                    let directives = self.rir.get_directives(*directives_start, *directives_len);
                    let is_copy = self.has_copy_directive(&directives);

                    // Linear types cannot be @copy
                    if *is_linear && is_copy {
                        return Err(CompileError::new(
                            ErrorKind::LinearStructCopy(struct_name.clone()),
                            inst.span,
                        ));
                    }

                    // Create placeholder struct def (fields will be resolved in phase 2)
                    let struct_def = StructDef {
                        name: struct_name,
                        fields: Vec::new(), // Filled in during resolve_declarations
                        is_copy,
                        is_linear: *is_linear,
                        destructor: None,  // Filled in during resolve_declarations
                        is_builtin: false, // User-defined struct
                        is_pub: *is_pub,
                        file_id: inst.span.file_id,
                    };

                    // Register in type pool and get pool-based StructId
                    let (struct_id, _) = self.type_pool.register_struct(*name, struct_def);

                    // Register in struct lookup with pool-based StructId
                    self.structs.insert(*name, struct_id);
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
    pub(crate) fn resolve_declarations(&mut self) -> CompileResult<()> {
        self.resolve_struct_fields()?;
        self.resolve_enum_payloads()?;
        self.propagate_field_linearity();
        self.resolve_remaining_declarations()?;
        // Now that value constants are separated from module bindings, reject
        // any value-constant name that collides with a function/struct/enum
        // (order-independent E0436, spec 10.3:1/10.5:1, RUE-239).
        self.check_const_cross_kind_collisions()?;
        Ok(())
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
        let mut jobs: Vec<(Spur, u32, u32, Span)> = Vec::new();
        for (_, inst) in self.rir.iter() {
            if let InstData::EnumDecl {
                name,
                payloads_start,
                payloads_len,
                ..
            } = &inst.data
            {
                if *payloads_len > 0 {
                    jobs.push((*name, *payloads_start, *payloads_len, inst.span));
                }
            }
        }

        for (name, payloads_start, payloads_len, span) in jobs {
            let words = self.rir.get_extra(payloads_start, payloads_len).to_vec();
            // Decode per-variant payload type symbols.
            let mut variant_payloads: Vec<Vec<Type>> = Vec::new();
            let mut i = 0usize;
            while i < words.len() {
                let k = words[i] as usize;
                i += 1;
                let mut payload = Vec::with_capacity(k);
                for _ in 0..k {
                    let ty_sym = Spur::try_from_usize(words[i] as usize)
                        .expect("valid interned type symbol in payload region");
                    i += 1;
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

            let enum_id = *self.enums.get(&name).expect("enum registered in phase 1");
            let mut def = self.type_pool.enum_def(enum_id);
            def.variant_payloads = variant_payloads;
            self.type_pool.update_enum_def(enum_id, def);
        }
        Ok(())
    }

    /// Propagate linearity from fields to containing structs (infectious
    /// linearity, spec 3.8:57 / RUE-40).
    ///
    /// A struct with a field whose type carries a linear value (directly,
    /// through an array, or through a nested struct) must itself be linear:
    /// if the container could be implicitly dropped, the linear field would
    /// be silently dropped with it. Runs to a fixpoint so linearity flows
    /// through arbitrarily deep nestings. The causing field is recorded in
    /// [`Sema::infectious_linear`] for diagnostics.
    pub(crate) fn propagate_field_linearity(&mut self) {
        let struct_ids: Vec<StructId> = self.structs.values().copied().collect();
        loop {
            let mut changed = false;
            for &struct_id in &struct_ids {
                let def = self.type_pool.struct_def(struct_id);
                if def.is_linear {
                    continue;
                }
                let Some(cause) = def
                    .fields
                    .iter()
                    .find(|field| self.type_carries_linear(field.ty))
                else {
                    continue;
                };
                let cause = (cause.name.clone(), self.format_type_name(cause.ty));
                let mut def = def;
                def.is_linear = true;
                self.type_pool.update_struct_def(struct_id, def);
                self.infectious_linear.insert(struct_id, cause);
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    /// Resolve struct field types. Must run before @copy validation.
    pub(crate) fn resolve_struct_fields(&mut self) -> CompileResult<()> {
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                name,
                fields_start,
                fields_len,
                ..
            } = &inst.data
            {
                let name_str = self.interner.resolve(&*name).to_string();
                // Verify the struct exists in our lookup table
                if !self.structs.contains_key(name) {
                    return Err(CompileError::new(
                        ErrorKind::InternalError(
                            ice!(
                                "struct not found in struct map",
                                phase: "sema/declarations",
                                details: {
                                    "struct_name" => name_str.to_string()
                                }
                            )
                            .to_string(),
                        ),
                        inst.span,
                    ));
                }

                // Get the struct ID from the lookup table
                let struct_id = *self.structs.get(name).ok_or_else(|| {
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

                let struct_name = name_str.clone();
                let fields = self.rir.get_field_decls(*fields_start, *fields_len);

                // Check for duplicate field names
                let mut seen_fields: HashSet<Spur> = HashSet::new();
                for (field_name, _) in &fields {
                    if !seen_fields.insert(*field_name) {
                        let field_name_str = self.interner.resolve(&*field_name).to_string();
                        return Err(CompileError::new(
                            ErrorKind::DuplicateField {
                                struct_name,
                                field_name: field_name_str,
                            },
                            inst.span,
                        ));
                    }
                }

                // Resolve field types
                let mut resolved_fields = Vec::new();
                for (field_name, field_type) in &fields {
                    let field_ty = self.resolve_type(*field_type, inst.span)?;
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
                        name: self.interner.resolve(&*field_name).to_string(),
                        ty: field_ty,
                    });
                }

                // Update the struct definition in the pool with resolved fields
                let mut struct_def = self.type_pool.struct_def(struct_id);
                struct_def.fields = resolved_fields;
                self.type_pool.update_struct_def(struct_id, struct_def);
            }
        }
        Ok(())
    }

    /// Resolve @copy validation, destructors, functions, and methods.
    pub(crate) fn resolve_remaining_declarations(&mut self) -> CompileResult<()> {
        // Collect all method InstRefs from anonymous struct types
        // These need to be skipped during function declaration collection because:
        // - They may use `Self` type which requires struct context
        // - They are registered later during comptime evaluation with proper Self resolution
        let mut anon_struct_method_refs = std::collections::HashSet::new();
        // Also collect method InstRefs from named struct declarations. Inline
        // methods (including associated functions with no `self`) are collected
        // via `collect_struct_methods`, which binds `Self` to the enclosing
        // type. They must be skipped in the generic FnDecl branch below, whose
        // `collect_function_signature` has no struct context and would reject a
        // `Self` return/parameter type as an unknown type (RUE-123).
        let mut named_struct_method_refs = std::collections::HashSet::new();
        for (_, inst) in self.rir.iter() {
            match &inst.data {
                InstData::AnonStructType {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    for method_ref in method_refs {
                        anon_struct_method_refs.insert(method_ref);
                    }
                }
                InstData::StructDecl {
                    methods_start,
                    methods_len,
                    ..
                } => {
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    for method_ref in method_refs {
                        named_struct_method_refs.insert(method_ref);
                    }
                }
                _ => {}
            }
        }

        // Pre-scan const declarations: collection is dependency-ordered
        // (an initializer may reference a constant declared later, even in
        // another file), so all pending declarations must be known up front.
        let mut const_collector = self.prescan_const_declarations()?;

        // First pass: collect all declarations and validate @copy structs
        for (inst_ref, inst) in self.rir.iter() {
            match &inst.data {
                InstData::StructDecl {
                    directives_start,
                    directives_len,
                    name,
                    methods_start,
                    methods_len,
                    ..
                } => {
                    self.validate_copy_struct(
                        *directives_start,
                        *directives_len,
                        *name,
                        inst.span,
                    )?;
                    // Collect methods defined inline in the struct
                    self.collect_struct_methods(*name, *methods_start, *methods_len, inst.span)?;
                }

                InstData::DropFnDecl { type_name, .. } => {
                    self.collect_destructor(*type_name, inst.span)?;
                }

                InstData::FnDecl {
                    is_pub,
                    is_unchecked,
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    ..
                } => {
                    // Skip methods (has_self = true) - these are handled elsewhere:
                    // - Named struct methods are collected via ImplDecl
                    if *has_self {
                        continue;
                    }

                    // Skip ALL methods from anonymous structs (including associated functions)
                    // These are registered during comptime evaluation with proper Self type context
                    if anon_struct_method_refs.contains(&inst_ref) {
                        continue;
                    }

                    // Skip named-struct associated functions (no `self`): they are
                    // collected via `collect_struct_methods` with `Self` bound to
                    // the enclosing type (RUE-123). Collecting them here as free
                    // functions would reject a `Self` signature type.
                    if named_struct_method_refs.contains(&inst_ref) {
                        continue;
                    }
                    self.collect_function_signature(
                        *name,
                        *params_start,
                        *params_len,
                        *return_type,
                        *body,
                        inst.span,
                        *is_pub,
                        *is_unchecked,
                    )?;
                }

                InstData::ConstDecl { name, .. } => {
                    // May already be collected: another constant's
                    // initializer can pull declarations in early (the
                    // collector is dependency-ordered, RUE-171).
                    self.collect_const_by_key((inst.span.file_id, *name), &mut const_collector)?;
                }

                _ => {}
            }
        }

        Ok(())
    }

    /// Validate that a @copy struct only contains Copy type fields.
    fn validate_copy_struct(
        &self,
        directives_start: u32,
        directives_len: u32,
        name: Spur,
        span: Span,
    ) -> CompileResult<()> {
        let directives = self.rir.get_directives(directives_start, directives_len);
        if !self.has_copy_directive(&directives) {
            return Ok(());
        }

        let struct_name = self.interner.resolve(&name).to_string();
        // Verify struct exists in our lookup
        if !self.structs.contains_key(&name) {
            return Err(CompileError::new(
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
            ));
        }

        // Get the struct ID from the lookup table
        let struct_id = *self.structs.get(&name).ok_or_else(|| {
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

    /// Collect a destructor definition and register it with its struct.
    fn collect_destructor(&mut self, type_name: Spur, span: Span) -> CompileResult<()> {
        let type_name_str = self.interner.resolve(&type_name).to_string();

        // Verify the struct exists
        if !self.structs.contains_key(&type_name) {
            return Err(CompileError::new(
                ErrorKind::DestructorUnknownType {
                    type_name: type_name_str,
                },
                span,
            ));
        }

        // Get the struct ID from the lookup table
        let struct_id = *self.structs.get(&type_name).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(
                    ice!(
                        "struct not found during destructor collection",
                        phase: "sema/declarations",
                        details: {
                            "struct_name" => type_name_str.to_string()
                        }
                    )
                    .to_string(),
                ),
                span,
            )
        })?;

        let mut struct_def = self.type_pool.struct_def(struct_id);
        if struct_def.destructor.is_some() {
            return Err(CompileError::new(
                ErrorKind::DuplicateDestructor {
                    type_name: type_name_str,
                },
                span,
            ));
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
        struct_def.destructor = Some(destructor_name);
        self.type_pool.update_struct_def(struct_id, struct_def);
        self.destructor_spans.insert(struct_id, span);
        Ok(())
    }

    /// Find the span of the `@copy` directive on the struct declaration
    /// named `type_name`, for diagnostics that point at the attribute.
    fn find_copy_directive_span(&self, type_name: Spur) -> Option<Span> {
        let copy_sym = self.interner.get("copy")?;
        for (_, inst) in self.rir.iter() {
            if let InstData::StructDecl {
                name,
                directives_start,
                directives_len,
                ..
            } = &inst.data
            {
                if *name != type_name {
                    continue;
                }
                let directives = self.rir.get_directives(*directives_start, *directives_len);
                return directives
                    .iter()
                    .find(|d| d.name == copy_sym)
                    .map(|d| d.span);
            }
        }
        None
    }

    /// Collect a function signature for forward reference.
    fn collect_function_signature(
        &mut self,
        name: Spur,
        params_start: u32,
        params_len: u32,
        return_type_sym: Spur,
        body: InstRef,
        span: Span,
        is_pub: bool,
        is_unchecked: bool,
    ) -> CompileResult<()> {
        // Reject user functions whose name collides with a runtime/codegen helper
        // symbol (e.g. `__rue_String_len`, `__rue_alloc`, `_start`). Without this, such a
        // definition either fails to link with a confusing duplicate-symbol error or
        // silently binds calls to the runtime's definition instead of the user's.
        let name_str = self.interner.resolve(&name);
        if rue_builtins::is_reserved_function_name(name_str) {
            return Err(CompileError::new(
                ErrorKind::ReservedFunctionName {
                    function_name: name_str.to_string(),
                },
                span,
            ));
        }

        let params = self.rir.get_params(params_start, params_len);

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
            Type::COMPTIME_TYPE
        } else {
            self.resolve_type(return_type_sym, span)?
        };

        // Allocate parameter data in the arena
        let params_range = self.param_arena.alloc(
            param_names.into_iter(),
            param_types.into_iter(),
            param_modes.into_iter(),
            param_comptime.into_iter(),
        );

        self.functions.insert(
            name,
            FunctionInfo {
                params: params_range,
                return_type: ret_type,
                return_type_sym,
                body,
                rir_params_start: params_start,
                rir_params_len: params_len,
                span,
                is_generic,
                is_pub,
                is_unchecked,
                file_id: span.file_id,
            },
        );
        Ok(())
    }

    /// Collect methods defined inline in a struct.
    fn collect_struct_methods(
        &mut self,
        type_name: Spur,
        methods_start: u32,
        methods_len: u32,
        span: Span,
    ) -> CompileResult<()> {
        let struct_id = match self.structs.get(&type_name) {
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

        let methods = self.rir.get_inst_refs(methods_start, methods_len);
        for method_ref in methods {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name: method_name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                self_mode,
                ..
            } = &method_inst.data
            {
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

                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
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
                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        self_mode: *self_mode,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
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
    ///   stored in `module_bindings` keyed by the declaring file — two files
    ///   binding the same name, even to different modules, is fine (RUE-113).
    /// - **Value constants** — everything else: the initializer is evaluated
    ///   through the comptime engine (`sema::comptime_eval`), so negated
    ///   literals, arithmetic, and any other comptime-evaluable expression
    ///   are legal initializers (RUE-171). Value constants keep the flat
    ///   global namespace shared with functions/types, so duplicates across
    ///   files are still E0418.
    ///
    /// Collection is on-demand: an initializer that references another
    /// not-yet-collected constant collects that constant first (see
    /// [`ConstCollector`]), so declaration order — within a file or across
    /// files — does not matter. Cyclic initializers are E0461.
    fn collect_const_by_key(
        &mut self,
        key: (FileId, Spur),
        st: &mut ConstCollector,
    ) -> CompileResult<()> {
        if st.done.contains(&key) {
            return Ok(());
        }
        if st.in_progress.contains(&key) {
            // Re-entering a key that is mid-evaluation: the initializers
            // form a cycle. Report it (never loop on it).
            let pos = st
                .in_progress
                .iter()
                .position(|k| k == &key)
                .expect("key was just found in in_progress");
            let cycle = st.in_progress[pos..]
                .iter()
                .chain(std::iter::once(&key))
                .map(|(_, n)| self.interner.resolve(n))
                .collect::<Vec<_>>()
                .join(" -> ");
            let span = st.pending[&key].span;
            return Err(CompileError::new(
                ErrorKind::ConstInitializerCycle { cycle },
                span,
            ));
        }
        // Not a pending const declaration (already-collected keys were
        // handled above): nothing to do.
        let Some(p) = st.pending.get(&key).copied() else {
            return Ok(());
        };
        let (file_id, name) = key;
        let name_str = self.interner.resolve(&name).to_string();

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

        st.in_progress.push(key);
        let outcome = self.eval_const_initializer(p.init, file_id, st, declared_ty);
        st.in_progress.pop();
        let outcome = outcome?;
        st.done.insert(key);

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
                self.module_bindings.insert(
                    (file_id, name),
                    ConstInfo {
                        is_pub: p.is_pub,
                        ty: module_ty,
                        init: p.init,
                        value: ConstValue::Type(module_ty),
                        span: p.span,
                    },
                );
            }
            ConstInit::Value(value) => {
                // Value constants share one global namespace (10.5:1): a
                // second declaration anywhere is E0418. (Same-file pairs of
                // any kind were already caught by the pre-scan.)
                if self.constants.contains_key(&name) {
                    return Err(CompileError::new(
                        ErrorKind::DuplicateConstant {
                            name: name_str,
                            kind: "constant".to_string(),
                        },
                        p.span,
                    ));
                }
                let ty = self.const_type_for_value(value, p.ty_sym, p.init, p.span, &name_str)?;
                self.constants.insert(
                    name,
                    ConstInfo {
                        is_pub: p.is_pub,
                        ty,
                        init: p.init,
                        value,
                        span: p.span,
                    },
                );
            }
        }
        Ok(())
    }

    /// Pre-scan the RIR for every `const` declaration, building the
    /// [`ConstCollector`] worklist. Two declarations of the same name in the
    /// same file are always a duplicate (E0418), whatever their kinds.
    fn prescan_const_declarations(&mut self) -> CompileResult<ConstCollector> {
        let mut st = ConstCollector {
            pending: HashMap::new(),
            by_name: HashMap::new(),
            done: HashSet::new(),
            in_progress: Vec::new(),
        };
        for (_, inst) in self.rir.iter() {
            if let InstData::ConstDecl {
                is_pub,
                name,
                ty,
                init,
                ..
            } = &inst.data
            {
                let key = (inst.span.file_id, *name);
                let pending = PendingConst {
                    is_pub: *is_pub,
                    ty_sym: *ty,
                    init: *init,
                    span: inst.span,
                };
                if st.pending.insert(key, pending).is_some() {
                    return Err(CompileError::new(
                        ErrorKind::DuplicateConstant {
                            name: self.interner.resolve(name).to_string(),
                            kind: "constant".to_string(),
                        },
                        inst.span,
                    ));
                }
                st.by_name.entry(*name).or_default().push(key);
            }
        }
        Ok(st)
    }

    /// Collect (on demand) every constant declaration that the name `name`,
    /// referenced from `referencing_file`, could resolve to: the same-file
    /// declaration (module bindings are per-file scoped) and any other file's
    /// declaration (value constants share one global namespace, and the
    /// defining file may not have been walked yet — command-line file order
    /// is arbitrary).
    fn ensure_const_collected(
        &mut self,
        name: Spur,
        referencing_file: FileId,
        st: &mut ConstCollector,
    ) -> CompileResult<()> {
        let same_file_key = (referencing_file, name);
        if st.pending.contains_key(&same_file_key) {
            self.collect_const_by_key(same_file_key, st)?;
        }
        let keys = st.by_name.get(&name).cloned().unwrap_or_default();
        for key in keys {
            self.collect_const_by_key(key, st)?;
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
        st: &mut ConstCollector,
        declared_ty: Option<Type>,
    ) -> CompileResult<ConstInit> {
        let init_inst = self.rir.get(init);
        let span = init_inst.span;

        match &init_inst.data {
            // @import("path") evaluates to a module at compile time.
            InstData::Intrinsic {
                name,
                args_start,
                args_len,
            } => {
                if *name == self.known.import {
                    // Validate exactly one argument
                    if *args_len != 1 {
                        return Err(CompileError::new(
                            ErrorKind::IntrinsicWrongArgCount {
                                name: "import".to_string(),
                                expected: 1,
                                found: *args_len as usize,
                            },
                            span,
                        ));
                    }

                    // Get the string literal argument
                    let arg_refs = self.rir.get_inst_refs(*args_start, *args_len);
                    let arg_inst = self.rir.get(arg_refs[0]);
                    let import_path = match &arg_inst.data {
                        InstData::StringConst(path_spur) => {
                            self.interner.resolve(path_spur).to_string()
                        }
                        _ => {
                            return Err(CompileError::new(
                                ErrorKind::ImportRequiresStringLiteral,
                                arg_inst.span,
                            ));
                        }
                    };

                    // Resolve the import path to an absolute file path
                    let resolved_path = self.resolve_import_path(&import_path, span)?;

                    // Register the module in the registry
                    let (module_id, _is_new) = self
                        .module_registry
                        .get_or_create(import_path, resolved_path);

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
            InstData::VarRef { name } => {
                let name = *name;
                self.ensure_const_collected(name, file_id, st)?;
                if let Some(binding) = self.module_bindings.get(&(file_id, name)) {
                    // `const m2 = m;` — aliasing a module binding declared in
                    // this file yields the same module (RUE-160).
                    return Ok(ConstInit::Module(binding.ty));
                }
                if let Some(info) = self.constants.get(&name) {
                    // Privacy (E0460, RUE-183): the value-constant table is
                    // global, so this alias could otherwise read a private
                    // constant from another directory. The initializer's own
                    // span locates the referencing file.
                    self.check_unqualified_visibility(
                        "constant",
                        self.interner.resolve(&name),
                        info.span.file_id,
                        info.is_pub,
                        span,
                    )?;
                    return Ok(ConstInit::Value(info.value));
                }
                // Not a constant: let the comptime engine decide (it rejects
                // unknown names and type names as non-evaluable).
                self.eval_const_value_expr(init, file_id, st, span, declared_ty)
            }

            // Member access: `base.member` where `base` is a module —
            // `const math = std.math;` (alias of a re-export, RUE-160) or
            // `const X: i32 = m.ANSWER;` (a member value constant).
            InstData::FieldGet { base, field } => {
                let (base, field) = (*base, *field);
                match self.eval_const_initializer(base, file_id, st, None)? {
                    ConstInit::Module(module_ty) => {
                        let module_id = module_ty
                            .as_module()
                            .expect("ConstInit::Module holds a module type");
                        self.resolve_module_member_const(module_id, field, file_id, span, st)
                    }
                    ConstInit::Value(_) => Err(CompileError::new(
                        ErrorKind::ConstExprNotSupported {
                            expr_kind: "member access on a non-module value".to_string(),
                        },
                        span,
                    )),
                }
            }

            // String constants would need the String type; not supported in
            // const context yet.
            InstData::StringConst(_) => Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: "string literals".to_string(),
                },
                span,
            )),

            // Everything else: literals, arithmetic, comptime blocks, ... —
            // evaluated by the comptime engine.
            _ => self.eval_const_value_expr(init, file_id, st, span, declared_ty),
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
        st: &mut ConstCollector,
        span: Span,
        declared_ty: Option<Type>,
    ) -> CompileResult<ConstInit> {
        // Pre-collect referenced constants: the engine's file-level-constant
        // lookup only consults the finished `constants` table, so anything
        // this initializer names must be collected first. (Also required
        // before type inference so referenced constants' types are known.)
        self.ensure_const_init_deps_collected(init, file_id, st)?;

        // Infer operand types for the initializer expression so the engine
        // checks arithmetic at the operand type instead of the raw-i64
        // fallback (RUE-230). Reports E0206 on a mismatched-operand-type
        // binary op before evaluation, mirroring the runtime path.
        let mut resolved_types: HashMap<InstRef, Type> = HashMap::new();
        self.infer_const_init_types(init, declared_ty, &mut resolved_types)?;

        let mut env = super::comptime_eval::ComptimeEnv::for_const_init(&resolved_types);
        match self.eval_const_expr(init, &mut env)? {
            // Type values (e.g. `const T = i32;`) are not supported as
            // constants: nothing can materialize them at a use site yet.
            Some(ConstValue::Type(_)) => Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: "a type value".to_string(),
                },
                span,
            )),
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
            InstData::VarRef { name } => self
                .constants
                .get(name)
                .map(|info| info.ty)
                .filter(|t| t.is_integer()),

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
                    self.infer_const_init_types(operand, expected, map)?
                }
            }

            // Logical NOT is a bool; still walk the operand for nested checks.
            InstData::Not { operand } => {
                let operand = *operand;
                self.infer_const_init_types(operand, None, map)?;
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
                let lt = self.infer_const_init_types(lhs, expected, map)?;
                let rt = self.infer_const_init_types(rhs, expected, map)?;
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
                let lt = self.infer_const_init_types(lhs, expected, map)?;
                self.infer_const_init_types(rhs, None, map)?;
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
                let lt = self.infer_const_init_types(lhs, None, map)?;
                let rt = self.infer_const_init_types(rhs, None, map)?;
                if lt.is_some() && rt.is_none() {
                    self.infer_const_init_types(rhs, lt, map)?;
                } else if rt.is_some() && lt.is_none() {
                    self.infer_const_init_types(lhs, rt, map)?;
                }
                None
            }

            InstData::And { lhs, rhs } | InstData::Or { lhs, rhs } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.infer_const_init_types(lhs, None, map)?;
                self.infer_const_init_types(rhs, None, map)?;
                None
            }

            // `comptime { expr }` forwards the context to its inner expression.
            InstData::Comptime { expr: inner } => {
                let inner = *inner;
                self.infer_const_init_types(inner, expected, map)?
            }

            // A block's tail expression carries the context; `let` initializers
            // are typed by their own value (no annotation context available
            // here), and non-tail statements carry no expected type.
            InstData::Block { extra_start, len } => {
                let stmt_refs: Vec<InstRef> = self
                    .rir
                    .get_extra(*extra_start, *len)
                    .iter()
                    .map(|&raw| InstRef::from_raw(raw))
                    .collect();
                let n = stmt_refs.len();
                let mut tail_ty = None;
                for (i, &stmt_ref) in stmt_refs.iter().enumerate() {
                    let is_tail = i + 1 == n;
                    let stmt_expected = if is_tail { expected } else { None };
                    let t = if let InstData::Alloc { init, .. } = &self.rir.get(stmt_ref).data {
                        let init = *init;
                        self.infer_const_init_types(init, None, map)?;
                        None
                    } else {
                        self.infer_const_init_types(stmt_ref, stmt_expected, map)?
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
    /// constant it references, so the comptime engine sees them all in the
    /// `constants` table regardless of declaration order.
    ///
    /// Only the expression forms the engine can evaluate are walked; anything
    /// else makes the expression non-evaluable anyway. A block-local `let`
    /// that shadows a constant name still triggers collection of the constant
    /// — harmless, since every constant is collected eventually.
    fn ensure_const_init_deps_collected(
        &mut self,
        expr: InstRef,
        file_id: FileId,
        st: &mut ConstCollector,
    ) -> CompileResult<()> {
        match &self.rir.get(expr).data {
            InstData::VarRef { name } => {
                let name = *name;
                self.ensure_const_collected(name, file_id, st)
            }
            InstData::Neg { operand }
            | InstData::Not { operand }
            | InstData::BitNot { operand } => {
                let operand = *operand;
                self.ensure_const_init_deps_collected(operand, file_id, st)
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
                self.ensure_const_init_deps_collected(lhs, file_id, st)?;
                self.ensure_const_init_deps_collected(rhs, file_id, st)
            }
            InstData::Comptime { expr: inner } => {
                let inner = *inner;
                self.ensure_const_init_deps_collected(inner, file_id, st)
            }
            InstData::Block { extra_start, len } => {
                let stmt_refs: Vec<InstRef> = self
                    .rir
                    .get_extra(*extra_start, *len)
                    .iter()
                    .map(|&raw| InstRef::from_raw(raw))
                    .collect();
                for stmt_ref in stmt_refs {
                    self.ensure_const_init_deps_collected(stmt_ref, file_id, st)?;
                }
                Ok(())
            }
            InstData::Alloc { init, .. } => {
                let init = *init;
                self.ensure_const_init_deps_collected(init, file_id, st)
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
        st: &mut ConstCollector,
    ) -> CompileResult<ConstInit> {
        let module_def = self.module_registry.get_def(module_id);
        let import_path = module_def.import_path.clone();
        let member_str = self.interner.resolve(&member).to_string();

        // Resolve the module's file by canonical FileId so equivalent path
        // spellings (`helper.rue` vs `./helper.rue`) refer to the same module
        // (spec 10.2:4, RUE-240), then use that file's stored path for
        // downstream directory-based visibility checks.
        let module_file_id = self.canonical_file_id(&module_def.file_path);
        let module_file_path = module_file_id
            .and_then(|id| self.get_file_path(id))
            .map(str::to_string)
            .unwrap_or(module_def.file_path);

        // Collect the member's declaration on demand (the module's file may
        // appear later in the declaration walk).
        if let Some(mfile) = module_file_id {
            self.collect_const_by_key((mfile, member), st)?;
        }

        // A module-binding member (re-export or alias) yields its module.
        if let Some(mfile) = module_file_id {
            if let Some(info) = self.module_bindings.get(&(mfile, member)) {
                let (is_pub, ty) = (info.is_pub, info.ty);
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                return Ok(ConstInit::Module(ty));
            }
        }

        // A value constant declared in the module's file yields its value.
        if let Some(info) = self.constants.get(&member) {
            if module_file_id == Some(info.span.file_id) {
                let (is_pub, value) = (info.is_pub, info.value);
                self.check_const_member_visibility(
                    is_pub,
                    &member_str,
                    &module_file_path,
                    accessing_file,
                    span,
                )?;
                return Ok(ConstInit::Value(value));
            }
        }

        // A function member is a value the constant machinery cannot hold:
        // there is no function type or function const-value yet (fn-valued
        // constants are a type-system gap, RUE-173). Diagnose it
        // precisely rather than "unknown member". The RIR is scanned directly
        // because function signatures are collected in the same declaration
        // walk and may not have been reached yet.
        if let Some(mfile) = module_file_id {
            let is_fn = self.rir.iter().any(|(_, inst)| {
                matches!(&inst.data, InstData::FnDecl { name, .. } if *name == member)
                    && inst.span.file_id == mfile
            });
            if is_fn {
                return Err(CompileError::new(
                    ErrorKind::ConstExprNotSupported {
                        expr_kind: "a function reference".to_string(),
                    },
                    span,
                ));
            }
        }

        // A struct/enum member would make this a type-valued constant, which
        // is not supported (same as `const T = i32;`).
        if self.structs.contains_key(&member) || self.enums.contains_key(&member) {
            return Err(CompileError::new(
                ErrorKind::ConstExprNotSupported {
                    expr_kind: "a type value".to_string(),
                },
                span,
            ));
        }

        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: import_path,
                member_name: member_str,
            },
            span,
        ))
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
            // Type values are rejected before this point (and module
            // bindings never take this path); keep the type if one slips
            // through so the mismatch error below names it.
            ConstValue::Type(t) => t,
        };

        let Some(ty_sym) = ty_sym else {
            // Type values are not annotatable (no syntax names them), so a
            // hypothetical unannotated type-valued constant is not an
            // annotation error; today they are rejected upstream anyway.
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

        if declared == inferred {
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

/// A constant declaration captured by the pre-scan, waiting to be collected.
#[derive(Clone, Copy)]
struct PendingConst {
    is_pub: bool,
    ty_sym: Option<Spur>,
    init: InstRef,
    span: Span,
}

/// State for dependency-ordered constant collection (RUE-171).
///
/// Constants may reference other constants regardless of declaration order
/// (including across files, where command-line order is arbitrary), so
/// collection is on-demand: evaluating an initializer that names another
/// not-yet-collected constant first collects that constant recursively.
/// `in_progress` is the active evaluation stack; re-entering a key already
/// on the stack is a cycle, reported as E0461 (never looped on).
pub(crate) struct ConstCollector {
    /// Every const declaration in the program, keyed per-file (two files may
    /// declare module bindings of the same name, RUE-113).
    pending: HashMap<(FileId, Spur), PendingConst>,
    /// name -> declaring keys, for resolving cross-file references.
    by_name: HashMap<Spur, Vec<(FileId, Spur)>>,
    /// Keys whose collection finished.
    done: HashSet<(FileId, Spur)>,
    /// Active evaluation stack (cycle detection).
    in_progress: Vec<(FileId, Spur)>,
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
