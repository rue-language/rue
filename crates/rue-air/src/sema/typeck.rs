//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use std::collections::HashMap;

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
            TypeKind::Struct(struct_id) => self.type_pool.struct_def(struct_id).name.clone(),
            TypeKind::Enum(enum_id) => self.type_pool.enum_def(enum_id).name.clone(),
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
    /// Resolve a type symbol to a Type.
    ///
    /// Handles array types with the syntax "[T; N]".
    /// Recognize slice type syntax `[T]` (ADR-0043, RUE-322).
    ///
    /// A slice is a bracketed type that is *not* a fixed array `[T; N]` (which
    /// [`parse_array_type_syntax`] handles). Returns `Some(result)` when the
    /// canonical type string is slice syntax so the caller short-circuits;
    /// `None` otherwise (a genuinely unknown type falls through to E0204).
    ///
    /// This gates the surface behind `--preview slices` and, when enabled,
    /// resolves the slice to its synthetic fat-pointer struct
    /// (see [`Self::get_or_create_slice_struct`]) so it flows through the
    /// existing aggregate ABI, field, and pointer codegen paths (ADR-0043
    /// Phase 1 runtime, RUE-322). Escape positions (return / field / binding)
    /// are rejected by [`Self::reject_slice_escape`] before reaching here.
    fn try_resolve_slice_type(
        &mut self,
        type_name: &str,
        span: Span,
    ) -> Option<CompileResult<Type>> {
        if type_name.starts_with('[')
            && type_name.ends_with(']')
            && parse_array_type_syntax(type_name).is_none()
        {
            Some(
                self.require_preview(PreviewFeature::Slices, "the slice type `[T]`", span)
                    .and_then(|()| self.get_or_create_slice_struct(type_name, span)),
            )
        } else {
            None
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
    /// The element type is parsed out of the bracket syntax and resolved first;
    /// the pointer field is `ptr const T` so that `s[i]` can lower to
    /// `@ptr_read(@ptr_offset(ptr, i))` with the correct element stride.
    fn get_or_create_slice_struct(&mut self, type_name: &str, span: Span) -> CompileResult<Type> {
        use crate::types::{StructDef, StructField};

        let type_sym = self.interner.get_or_intern(type_name);
        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            return Ok(Type::new_struct(struct_id));
        }

        // Parse `[T]` -> element type name `T` and resolve it.
        let element_name = type_name[1..type_name.len() - 1].trim().to_string();
        let element_sym = self.interner.get_or_intern(&element_name);
        let element_ty = self.resolve_type(element_sym, span)?;
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
                let mut def = def.clone();
                def.destructor = Some(qualified);
                self.type_pool.update_struct_def(struct_id, def);
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
    /// (`Type::method`) of `struct_id`, file-qualified when the type name is
    /// ambiguous (RUE-571). Definition sites (function-body analysis) and
    /// call sites (method / assoc-fn call analysis) must both build the name
    /// through this helper so they meet in AIR/codegen.
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

    /// Symbol for the destructor of `struct_id` (`Type.__drop`),
    /// file-qualified when the type name is ambiguous (RUE-571).
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
    fn get_or_create_str_fixed_struct(&mut self, capacity: u64, span: Span) -> CompileResult<Type> {
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

    /// Resolve a type-name symbol that may be a comptime-type alias
    /// (`const A = Pair(i32);` used as a type). Returns the aliased type, or
    /// `None` if the name is not a `type`-valued constant.
    ///
    /// During declaration binding, an indexed dependency may be resolved before
    /// the source-order sweep reaches it. Body analysis only reads the completed
    /// constant namespace.
    pub(crate) fn resolve_const_type_alias(
        &mut self,
        type_sym: Spur,
        span: Span,
    ) -> CompileResult<Option<Type>> {
        let mut value = self
            .resolve_const_info_in_file(type_sym, span.file_id)
            .map(|info| info.value);
        if value.is_none() && self.declaration_binding_active {
            value = self.try_resolve_indexed_const_during_binding(type_sym, span.file_id);
        }
        let Some(ConstValue::Type(alias_ty)) = value else {
            return Ok(None);
        };
        // Privacy (E0460): an unqualified alias must not reach a private constant
        // in another directory. Re-read the (now-collected) info for its origin.
        if let Some(info) = self.resolve_const_info_in_file(type_sym, span.file_id) {
            let (file_id, is_pub) = (info.span.file_id, info.is_pub);
            let type_name = self.interner.resolve(&type_sym).to_string();
            self.check_unqualified_visibility("constant", &type_name, file_id, is_pub, span)?;
            self.record_resolved_declaration_type_target(
                file_id,
                type_name,
                super::DeclarationTypeDependencyTargetKind::ValueConst,
            );
        }
        Ok(Some(alias_ty))
    }

    pub(crate) fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let ty = self.resolve_type_inner(type_sym, span)?;
        self.record_resolved_declaration_type(ty);
        Ok(ty)
    }

    fn resolve_type_inner(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        // Own the name so the read-only branches below borrow this local rather
        // than `self.interner`, leaving `self` free for the `&mut self` slice
        // path (`try_resolve_slice_type` lazily registers a synthetic struct).
        let type_name_owned = self.interner.resolve(&type_sym).to_string();
        let type_name = type_name_owned.as_str();

        // Check primitive types first (single shared table, RUE-155).
        // Note: String is handled below via struct lookup (it's a builtin struct).
        if let Some(ty) = Type::from_primitive_name(type_name) {
            return Ok(ty);
        }

        // The `str` string type (ADR-0043): `[u8]` + UTF-8, represented as a
        // first-class 2-word fat-pointer struct.
        if type_name == "str" {
            return self.get_or_create_str_struct(span);
        }

        if let Some(struct_id) = self
            .structs_by_file_name
            .get(&(span.file_id, type_sym))
            .copied()
            .or_else(|| self.resolve_builtin_struct_name(type_sym))
        {
            // Privacy (E0460, RUE-183): an unqualified type reference must
            // not reach a private struct defined in another directory —
            // privacy is uniform across item kinds (spec 10.3:1, 10.3:7).
            // Spans here are always the reference site (annotation,
            // signature, struct literal), so `span.file_id` is the
            // referencing file.
            let struct_def = self.type_pool.struct_def(struct_id);
            self.check_unqualified_visibility(
                "struct",
                type_name,
                struct_def.file_id,
                struct_def.is_pub,
                span,
            )?;
            Ok(Type::new_struct(struct_id))
        } else if let Some(enum_id) = self
            .enums_by_file_name
            .get(&(span.file_id, type_sym))
            .copied()
            .or_else(|| self.resolve_builtin_enum_name(type_sym))
        {
            // Privacy (E0460, RUE-185): same rule for enums — an unqualified
            // type reference must not reach a private enum defined in another
            // directory.
            let enum_def = self.type_pool.enum_def(enum_id);
            self.check_unqualified_visibility(
                "enum",
                type_name,
                enum_def.file_id,
                enum_def.is_pub,
                span,
            )?;
            Ok(Type::new_enum(enum_id))
        } else if let Some(alias_ty) = self.resolve_const_type_alias(type_sym, span)? {
            Ok(alias_ty)
        } else {
            // Check for array type syntax: [T; N]
            if let Some((element_type, len)) = parse_array_type_syntax(type_name) {
                // Resolve the element type first
                let element_sym = self.interner.get_or_intern(&element_type);
                let element_ty = self.resolve_type(element_sym, span)?;
                // Resolve the length (literal, or a `const` / `comptime` value
                // parameter name) to a concrete value (RUE-16). No comptime
                // value substitution is available on this path; named lengths
                // resolve against file-level constants only.
                let length = self.resolve_array_length(&len, span, None)?;
                // Get or create the array type
                let array_type_id = self.get_or_create_array_type(element_ty, length);
                Ok(Type::new_array(array_type_id))
            } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr const ") {
                // Pointer type syntax: ptr const T
                let pointee_sym = self.interner.get_or_intern(pointee_type_str);
                let pointee_ty = self.resolve_type(pointee_sym, span)?;
                let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
                Ok(Type::new_ptr_const(ptr_type_id))
            } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr mut ") {
                // Pointer type syntax: ptr mut T
                let pointee_sym = self.interner.get_or_intern(pointee_type_str);
                let pointee_ty = self.resolve_type(pointee_sym, span)?;
                let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
                Ok(Type::new_ptr_mut(ptr_type_id))
            } else if let Some((call_name, arg_strs)) = parse_type_call_syntax(type_name) {
                // Fixed-capacity string `Str(N)` (ADR-0043 Phase 5, RUE-326):
                // the stable fixed string rung, `[u8; N]` + UTF-8. The capacity `N`
                // is a literal (`Str(8)`, produced by `TypeExpr::StrFixed`) or a
                // `const` name that resolved to a literal on the `TypeCall`
                // path; either way it arrives here as the single argument
                // string. It is reduced to a 2-word fat-pointer struct so it
                // flows through the existing `str`/slice paths.
                if call_name == "Str" {
                    return self.resolve_str_fixed_type(&call_name, &arg_strs, span);
                }
                if call_name.contains('.') {
                    return self
                        .resolve_qualified_type_function_call(&call_name, &arg_strs, span, None);
                }
                // A type-function application written directly in type position
                // (`Result(i32, i32)`; RUE-241). Reduce the comptime type call
                // to its monomorphized concrete type. No analysis context is
                // available on this context-free path, so arguments resolve
                // context-free (a signature/return position collected before any
                // body context exists).
                self.resolve_type_function_call(&call_name, &arg_strs, span, None)
            } else if let Some(result) = self.try_resolve_slice_type(type_name, span) {
                // Slice type `[T]` (ADR-0043, RUE-322): gated + not-yet-runnable.
                result
            } else if type_name.contains('.') {
                self.resolve_qualified_type_name(type_name, span)
            } else if let Some(alias_ty) = self.resolve_const_type_alias(type_sym, span)? {
                // A module-level `const Alias = SomeType;` used as a plain type
                // name — e.g. the field type of a top-level named struct in its
                // defining file (RUE-706). The struct/enum tables above don't
                // include const aliases, so consult the declaration-bound
                // constant namespace as well. The context-aware
                // `resolve_type_with_ctx` already does this; this brings the
                // context-free path to parity.
                Ok(alias_ty)
            } else {
                Err(CompileError::new(
                    ErrorKind::UnknownType(type_name.to_string()),
                    span,
                ))
            }
        }
    }

    pub(crate) fn record_resolved_declaration_type(&mut self, ty: Type) {
        match ty.kind() {
            TypeKind::Struct(id) => {
                let def = self.type_pool.struct_def(id);
                if !def.is_builtin && !def.name.starts_with("__anon_struct_") {
                    self.record_resolved_declaration_type_target(
                        def.file_id,
                        def.name,
                        super::DeclarationTypeDependencyTargetKind::Struct,
                    );
                }
            }
            TypeKind::Enum(id) => {
                let def = self.type_pool.enum_def(id);
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
        self.declaration_type_dependencies
            .push(super::DeclarationTypeDependencyEvent {
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
            });
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
    /// the CFG builder later sees (the pool is cloned *after* specialization,
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
        // A local comptime type variable (`let P = Point();`) or a substituted
        // comptime type parameter is a type value already — resolve it directly.
        if let Some(&ty) = ctx.comptime_type_vars.get(&type_sym) {
            return Ok(ty);
        }
        let type_name = self.interner.resolve(&type_sym);
        if let Some((element_type, len)) = parse_array_type_syntax(type_name) {
            let element_sym = self.interner.get_or_intern(&element_type);
            let element_ty = self.resolve_type_with_ctx(element_sym, span, ctx)?;
            let length = self.resolve_array_length(&len, span, Some(&ctx.comptime_value_vars))?;
            let array_type_id = self.get_or_create_array_type(element_ty, length);
            Ok(Type::new_array(array_type_id))
        } else if let Some(pointee) = type_name.strip_prefix("ptr const ") {
            let pointee_sym = self.interner.get_or_intern(pointee);
            let pointee_ty = self.resolve_type_with_ctx(pointee_sym, span, ctx)?;
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
            Ok(Type::new_ptr_const(ptr_type_id))
        } else if let Some(pointee) = type_name.strip_prefix("ptr mut ") {
            let pointee_sym = self.interner.get_or_intern(pointee);
            let pointee_ty = self.resolve_type_with_ctx(pointee_sym, span, ctx)?;
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
            Ok(Type::new_ptr_mut(ptr_type_id))
        } else if let Some((call_name, arg_strs)) = parse_type_call_syntax(type_name) {
            // Fixed-capacity string `Str(N)` (ADR-0043 Phase 5, RUE-326): the
            // context-aware annotation path (`let s: Str(8)`). Intercepted here,
            // as in the context-free `resolve_type`, before the general
            // type-function reduction so `Str` is not looked up as a `-> type`
            // constructor.
            if call_name == "Str" {
                return self.resolve_str_fixed_type(&call_name, &arg_strs, span);
            }
            if call_name.contains('.') {
                return self.resolve_qualified_type_function_call(
                    &call_name,
                    &arg_strs,
                    span,
                    Some(ctx),
                );
            }
            // A type-function application in an annotation position whose
            // arguments may name enclosing comptime type parameters or local
            // comptime type variables (`let x: Option(T)` / `let x: Option(P)`).
            // Thread `ctx` so each argument resolves through this same
            // context-aware resolver rather than context-free — otherwise the
            // inner `T`/`P` would miss and yield a spurious E0204 (RUE-272).
            self.resolve_type_function_call(&call_name, &arg_strs, span, Some(ctx))
        } else {
            // Non-composite leaf: primitive / struct / enum / unknown. Defer to
            // the context-free resolver for identical resolution, privacy
            // checks, and the E0204 on a genuinely-unknown name.
            self.resolve_type(type_sym, span)
        }
    }

    fn resolve_qualified_type_name(&mut self, path: &str, span: Span) -> CompileResult<Type> {
        let segments: Vec<&str> = path.split('.').collect();
        if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
            return Err(CompileError::new(
                ErrorKind::UnknownType(path.to_string()),
                span,
            ));
        }
        let (module_id, module_file_id, _module_file_path) =
            self.resolve_type_module_prefix(&segments[..segments.len() - 1], span)?;
        let member = segments[segments.len() - 1];
        let member_sym = self.interner.get_or_intern(member);

        if let Some(struct_id) = module_file_id.and_then(|file_id| {
            self.structs_by_file_name
                .get(&(file_id, member_sym))
                .copied()
        }) {
            let struct_def = self.type_pool.struct_def(struct_id);
            self.check_unqualified_visibility(
                "struct",
                member,
                struct_def.file_id,
                struct_def.is_pub,
                span,
            )?;
            return Ok(Type::new_struct(struct_id));
        }
        if let Some(enum_id) = module_file_id
            .and_then(|file_id| self.enums_by_file_name.get(&(file_id, member_sym)).copied())
        {
            let enum_def = self.type_pool.enum_def(enum_id);
            self.check_unqualified_visibility(
                "enum",
                member,
                enum_def.file_id,
                enum_def.is_pub,
                span,
            )?;
            return Ok(Type::new_enum(enum_id));
        }
        // A type-valued constant member (`module.Alias` where the module has
        // `pub const Alias = SomeType`). During declaration binding a member
        // referenced from a type position may not be resolved yet, so resolve
        // its indexed declaration in the MODULE's file
        // (`resolve_const_type_alias`); without this the member alias
        // resolved in value positions but was E0707 in field/param/return
        // positions (RUE-630).
        if let Some(file_id) = module_file_id
            && self.declaration_binding_active
            && self
                .constants_by_file_name
                .get(&(file_id, member_sym))
                .is_none()
        {
            self.try_resolve_indexed_const_during_binding(member_sym, file_id);
        }
        if let Some(info) = module_file_id
            .and_then(|file_id| self.constants_by_file_name.get(&(file_id, member_sym)))
            && let ConstValue::Type(alias_ty) = info.value
        {
            self.check_unqualified_visibility(
                "constant",
                member,
                info.span.file_id,
                info.is_pub,
                span,
            )?;
            return Ok(alias_ty);
        }

        let module_def = self.module_registry.get_def(module_id);
        Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: module_def.import_path.clone(),
                member_name: member.to_string(),
            },
            span,
        ))
    }

    fn resolve_qualified_type_function_call(
        &mut self,
        call_path: &str,
        arg_strs: &[String],
        span: Span,
        ctx: Option<&AnalysisContext>,
    ) -> CompileResult<Type> {
        let declaration_type_observer = self.declaration_type_observer.clone();
        let segments: Vec<&str> = call_path.split('.').collect();
        if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
            return Err(CompileError::new(
                ErrorKind::UnknownType(format!("{}(...)", call_path)),
                span,
            ));
        }
        let (_module_id, module_file_id, _module_file_path) =
            self.resolve_type_module_prefix(&segments[..segments.len() - 1], span)?;
        let member = segments[segments.len() - 1];
        let member_sym = self.interner.get_or_intern(member);
        if self.declaration_binding_active
            && let Some(module_file_id) = module_file_id
        {
            self.collect_free_function_signature_during_binding(member_sym, Some(module_file_id))?;
        }
        let Some(function_key) = module_file_id
            .and_then(|file_id| self.resolve_function_name_local(member_sym, file_id))
        else {
            return Err(CompileError::new(
                ErrorKind::UnknownType(format!("{}(...)", call_path)),
                span,
            ));
        };

        let Some(fn_info) = self
            .functions
            .get(&function_key)
            .copied()
            .filter(|info| module_file_id == Some(info.file_id))
        else {
            return Err(CompileError::new(
                ErrorKind::UnknownType(format!("{}(...)", call_path)),
                span,
            ));
        };
        self.declaration_type_observer = declaration_type_observer;
        self.record_declaration_type_call_head(function_key, fn_info);
        self.check_unqualified_visibility(
            "function",
            member,
            fn_info.file_id,
            fn_info.is_pub,
            span,
        )?;
        if !self.function_returns_type(&fn_info) {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "'{}' is not a type: only a function returning `type` (a type \
                         constructor) can be applied as a type here",
                        call_path
                    ),
                },
                span,
            ));
        }

        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        if arg_strs.len() != param_names.len()
            || !(param_names.is_empty() || param_comptime.iter().all(|&c| c))
        {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "type constructor '{}' expects {} comptime type argument(s), \
                         but {} were provided",
                        call_path,
                        param_names.len(),
                        arg_strs.len()
                    ),
                },
                span,
            ));
        }

        // Kind-aware binding: type args resolve as types, value args as
        // comptime constants (RUE-552).
        let (callee_types, callee_values) =
            self.bind_type_ctor_args(call_path, fn_info, arg_strs, span, ctx)?;
        match self
            .reduce_type_ctor_body(function_key, &callee_types, &callee_values)
            .map_err(|e| Self::label_ctor_instantiation_site(e, span))?
        {
            Some(ConstValue::Type(t)) => Ok(t),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "the type constructor '{}' did not reduce to a concrete type \
                         at compile time",
                        call_path
                    ),
                },
                span,
            )),
        }
    }

    /// Resolve a module-qualified type-constructor call (`m.Mk(T)`,
    /// `std.option.Option(T)`) appearing in a *comptime type position* during
    /// body reduction, under the current comptime substitutions (RUE-511).
    ///
    /// This is the comptime-evaluation analogue of
    /// [`resolve_qualified_type_function_call`]: same module-prefix walk,
    /// membership check, and visibility rule, but arguments resolve through the
    /// enclosing `type_subst`/`value_subst` (so `T` inside the constructor binds
    /// to the concrete element type) and every failure yields `None` rather than
    /// a diagnostic — the caller reports the comptime failure (E1200).
    ///
    /// Member resolution uses the receiver module's defining file. A default
    /// span carries no file context for the prefix walk, so it is treated as
    /// non-evaluable.
    ///
    /// [`resolve_qualified_type_function_call`]:
    /// Sema::resolve_qualified_type_function_call
    /// Comptime-path analogue of [`Self::resolve_type_ctor_value_arg`]
    /// (RUE-552): resolve one comptime VALUE argument of a type-constructor
    /// application under the current `value_subst` — an integer/bool literal,
    /// an enclosing comptime value parameter (`Buffer(M)` inside another
    /// constructor body), or a file-level constant. `None` (no diagnostics on
    /// this path) makes the enclosing type non-evaluable; the caller reports
    /// the comptime failure.
    fn resolve_comptime_type_ctor_value_arg(
        &mut self,
        arg: &str,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<ConstValue> {
        let text = arg.trim();
        if let Ok(n) = text.parse::<i128>() {
            return Some(ConstValue::Integer(n));
        }
        if text == "true" {
            return Some(ConstValue::Bool(true));
        }
        if text == "false" {
            return Some(ConstValue::Bool(false));
        }
        let sym = self.interner.get_or_intern(text);
        if let Some(v) = value_subst.get(&sym) {
            return Some(*v);
        }
        // A source `comptime T: type` parameter is itself a compile-time
        // value when the callee's dependent value parameter has concretely
        // resolved to `type` (`Witness(type, T)`). Keep source-kind routing
        // authoritative, but expose that value through the same binder once
        // the enclosing type substitution is concrete.
        if let Some(ty) = type_subst.get(&sym) {
            return Some(ConstValue::Type(*ty));
        }
        (span != Span::default())
            .then(|| self.resolve_const_info_in_file(sym, span.file_id))
            .flatten()
            .map(|info| info.value)
    }

    fn resolve_qualified_type_call_for_comptime(
        &mut self,
        call_path: &str,
        arg_strs: &[String],
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<Type> {
        if span == Span::default() {
            return None;
        }
        let segments: Vec<&str> = call_path.split('.').collect();
        if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
            return None;
        }
        let (_module_id, module_file_id, _module_file_path) = self
            .resolve_type_module_prefix(&segments[..segments.len() - 1], span)
            .ok()?;
        let module_file_id = module_file_id?;
        let member = segments[segments.len() - 1];
        let member_sym = self.interner.get_or_intern(member);
        if self.declaration_binding_active {
            self.collect_free_function_signature_during_binding(member_sym, Some(module_file_id))
                .ok()?;
        }
        let function_key = self.resolve_function_name_local(member_sym, module_file_id)?;
        // Membership (RUE-564 hole guard) + `-> type` + visibility.
        let fn_info = self
            .functions
            .get(&function_key)
            .copied()
            .filter(|info| info.file_id == module_file_id)?;
        if !self.function_returns_type(&fn_info) {
            return None;
        }
        self.check_unqualified_visibility(
            "function",
            member,
            fn_info.file_id,
            fn_info.is_pub,
            span,
        )
        .ok()?;

        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        if arg_strs.len() != param_names.len()
            || !(param_names.is_empty() || param_comptime.iter().all(|&c| c))
        {
            return None;
        }
        // Kind-aware binding (RUE-552): type parameters take type arguments;
        // comptime VALUE parameters take value arguments resolved under the
        // enclosing value substitution.
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let mut callee_types: HashMap<Spur, Type> = HashMap::new();
        let mut callee_values: HashMap<Spur, ConstValue> = HashMap::new();
        for (i, arg) in arg_strs.iter().enumerate() {
            if param_comptime_type[i] {
                let arg_sym = self.interner.get_or_intern(arg);
                let arg_ty = self.resolve_type_for_comptime_with_subst_and_values_at_span(
                    arg_sym,
                    type_subst,
                    value_subst,
                    span,
                )?;
                callee_types.insert(param_names[i], arg_ty);
            } else {
                let val =
                    self.resolve_comptime_type_ctor_value_arg(arg, type_subst, value_subst, span)?;
                callee_values.insert(param_names[i], val);
            }
        }
        match self
            .reduce_type_ctor_body(function_key, &callee_types, &callee_values)
            .ok()?
        {
            Some(ConstValue::Type(t)) => Some(t),
            _ => None,
        }
    }

    fn resolve_type_module_prefix(
        &mut self,
        segments: &[&str],
        span: Span,
    ) -> CompileResult<(crate::types::ModuleId, Option<FileId>, String)> {
        self.resolve_type_module_prefix_in_file(span.file_id, segments, span)
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
        let Some((first, rest)) = segments.split_first() else {
            return Err(CompileError::new(
                ErrorKind::UnknownType(String::new()),
                span,
            ));
        };
        let first_sym = self.interner.get_or_intern(first);
        let Some(binding) = self.resolve_module_binding_in_file(root_file, first_sym) else {
            return Err(CompileError::new(
                ErrorKind::UnknownType((*first).to_string()),
                span,
            ));
        };
        let mut module_id = binding
            .ty
            .as_module()
            .expect("module binding holds a module type");

        for segment in rest {
            let module_def = self.module_registry.get_def(module_id);
            let module_file_id = Some(module_def.file_id);
            let segment_sym = self.interner.get_or_intern(segment);
            let Some(module_file_id) = module_file_id else {
                return Err(CompileError::new(
                    ErrorKind::UnknownModuleMember {
                        module_name: module_def.import_path.clone(),
                        member_name: (*segment).to_string(),
                    },
                    span,
                ));
            };
            let Some(binding) = self.resolve_module_binding_in_file(module_file_id, segment_sym)
            else {
                return Err(CompileError::new(
                    ErrorKind::UnknownModuleMember {
                        module_name: module_def.import_path.clone(),
                        member_name: (*segment).to_string(),
                    },
                    span,
                ));
            };
            let (binding_is_pub, binding_ty) = (binding.is_pub, binding.ty);
            self.check_unqualified_visibility(
                "constant",
                segment,
                module_file_id,
                binding_is_pub,
                span,
            )?;
            module_id = binding_ty
                .as_module()
                .expect("module binding holds a module type");
        }

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
    fn resolve_module_binding_in_file(
        &mut self,
        file_id: FileId,
        name: Spur,
    ) -> Option<super::info::ConstInfo> {
        if let Some(binding) = self.module_bindings.get(&(file_id, name)) {
            return Some(binding.clone());
        }
        if !self.declaration_binding_active {
            return None;
        }

        self.try_resolve_indexed_const_during_binding(name, file_id);
        self.module_bindings.get(&(file_id, name)).cloned()
    }

    /// Resolve a type-function application written directly in type position
    /// (`Result(i32, i32)`; RUE-241) to its monomorphized concrete type.
    ///
    /// The callee must be a comptime `-> type` constructor already collected
    /// into the function table (constructors are conventionally declared before
    /// use, the same order the named-const form and value-position use require).
    /// Each argument string is resolved as a type (so nested calls like
    /// `Result(Option(i32), i32)` compose), then the constructor body is
    /// reduced under that substitution — yielding the identical type a
    /// value-position call (`let R = Result(i32, i32)`) or the named-const form
    /// (`const R: type = Result(i32, i32)`) produces.
    ///
    /// A call to a callee that is not a `-> type` function (a value-returning
    /// function, `fn f() -> some_value_fn()`), an arity mismatch, or a body
    /// that does not reduce to a type is reported cleanly as E1200 (an unknown
    /// callee as E0204), never a crash.
    ///
    /// When `ctx` is `Some`, each argument is resolved through
    /// [`resolve_type_with_ctx`] so an argument naming an enclosing comptime
    /// type parameter or a local comptime type variable (`Option(T)` inside a
    /// generic function, `let x: Option(P)` where `P` is a local type alias)
    /// resolves from the analysis context rather than being reported as an
    /// unknown type (RUE-272). When `ctx` is `None` (a signature/return
    /// position, collected before any body context exists) arguments resolve
    /// context-free, exactly as before.
    ///
    /// [`resolve_type_with_ctx`]: Sema::resolve_type_with_ctx
    /// Bind a type-constructor application's canonical argument strings to
    /// the callee's comptime parameters (RUE-552). A `comptime T: type`
    /// parameter takes a TYPE argument, resolved as a type; a comptime VALUE
    /// parameter (`comptime N: i32`) takes an integer/bool literal, an
    /// in-scope comptime value parameter, or a file-level constant — so
    /// `Buffer(2)`, `Buffer(K)`, and `Matrix(i32, 3)` all apply directly in
    /// type position (spec 4.14:22-23).
    fn bind_type_ctor_args(
        &mut self,
        call_display: &str,
        function: FunctionInfo,
        arg_strs: &[String],
        span: Span,
        ctx: Option<&AnalysisContext>,
    ) -> CompileResult<(HashMap<Spur, Type>, HashMap<Spur, ConstValue>)> {
        let params = function.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime_type = self.comptime_type_param_flags(&function);
        let mut callee_types: HashMap<Spur, Type> = HashMap::new();
        let mut callee_values: HashMap<Spur, ConstValue> = HashMap::new();
        for (i, arg) in arg_strs.iter().enumerate() {
            if param_comptime_type[i] {
                // A value where a type is expected gets a targeted message
                // instead of decaying to "unknown type '2'".
                if arg.trim().parse::<i128>().is_ok() {
                    return Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "argument '{}' of type constructor '{}' must be a type (this parameter is `comptime {}: type`)",
                                arg,
                                call_display,
                                self.interner.resolve(&param_names[i])
                            ),
                        },
                        span,
                    ));
                }
                let arg_sym = self.interner.get_or_intern(arg);
                let arg_ty = match ctx {
                    Some(ctx) => self.resolve_type_with_ctx(arg_sym, span, ctx)?,
                    None => self.resolve_type(arg_sym, span)?,
                };
                callee_types.insert(param_names[i], arg_ty);
            } else {
                let val = self.resolve_type_ctor_value_arg(call_display, arg, span, ctx)?;
                callee_values.insert(param_names[i], val);
            }
        }
        Ok((callee_types, callee_values))
    }

    /// Resolve one comptime VALUE argument of a type-constructor application
    /// in type position (RUE-552) from its canonical string: an integer
    /// literal (`2`, `-3`), a bool literal, an in-scope comptime value
    /// parameter (`Buffer(M)` inside another constructor body), or a
    /// file-level constant name. The comptime evaluator range-checks the
    /// value against the parameter's declared width during reduction, the
    /// same as a value-position application.
    fn resolve_type_ctor_value_arg(
        &mut self,
        call_display: &str,
        arg: &str,
        span: Span,
        ctx: Option<&AnalysisContext>,
    ) -> CompileResult<ConstValue> {
        let text = arg.trim();
        if let Ok(n) = text.parse::<i128>() {
            return Ok(ConstValue::Integer(n));
        }
        if text == "true" {
            return Ok(ConstValue::Bool(true));
        }
        if text == "false" {
            return Ok(ConstValue::Bool(false));
        }
        let sym = self.interner.get_or_intern(text);
        if let Some(ctx) = ctx
            && let Some(v) = ctx.comptime_value_vars.get(&sym)
        {
            return Ok(*v);
        }
        if let Some(info) = self.resolve_const_info_in_file(sym, span.file_id) {
            return Ok(info.value);
        }
        Err(CompileError::new(
            ErrorKind::ComptimeEvaluationFailed {
                reason: format!(
                    "argument '{}' of type constructor '{}' must be a compile-time known value (an integer or bool literal, a comptime parameter, or a constant)",
                    text, call_display
                ),
            },
            span,
        ))
    }

    fn resolve_type_function_call(
        &mut self,
        call_name: &str,
        arg_strs: &[String],
        span: Span,
        ctx: Option<&AnalysisContext>,
    ) -> CompileResult<Type> {
        let declaration_type_observer = self.declaration_type_observer.clone();
        let name_sym = self.interner.get_or_intern(call_name);
        // During declaration binding, the constructor may not be collected yet: struct-field and
        // enum-payload types, const initializers, and earlier function
        // signatures can resolve before the main declaration sweep reaches
        // the callee's `FnDecl`. Collect the same-file declaration on demand
        // so `struct S { v: Vec(i32) }` and signatures naming a later-collected
        // `Vec(i32)` resolve consistently (RUE-603), mirroring the
        // declaration-time indexed const-alias resolution path.
        let mut name_key = self.resolve_function_name_local(name_sym, span.file_id);
        if name_key.is_none() && self.declaration_binding_active {
            self.collect_free_function_signature_during_binding(name_sym, Some(span.file_id))?;
            name_key = self.resolve_function_name_local(name_sym, span.file_id);
        }
        let Some(name_key) = name_key else {
            return Err(CompileError::new(
                ErrorKind::UnknownType(format!("{}(...)", call_name)),
                span,
            ));
        };

        // The callee must be a known `-> type` constructor.
        let Some(fn_info) = self.functions.get(&name_key).copied() else {
            return Err(CompileError::new(
                ErrorKind::UnknownType(format!("{}(...)", call_name)),
                span,
            ));
        };
        self.declaration_type_observer = declaration_type_observer;
        self.record_declaration_type_call_head(name_key, fn_info);
        let is_type_ctor = self.function_returns_type(&fn_info);
        let params = fn_info.params;
        let ctor_file_id = fn_info.file_id;
        let ctor_is_pub = fn_info.is_pub;

        // Privacy (E0460, RUE-283): applying a type constructor in type-
        // annotation position (`let x: Secret(i32)`, a param/return type) is a
        // reference to that function and must obey the same uniform-privacy
        // rule the value/call path enforces (spec 10.3:7) — a non-`pub`
        // constructor defined in another directory is not usable here. Without
        // this, a private `-> type` constructor leaked across directories via a
        // type annotation (a privacy-soundness hole).
        self.check_unqualified_visibility("function", call_name, ctor_file_id, ctor_is_pub, span)?;
        if !is_type_ctor {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "'{}' is not a type: only a function returning `type` (a type \
                         constructor) can be applied as a type here",
                        call_name
                    ),
                },
                span,
            ));
        }

        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        if arg_strs.len() != param_names.len()
            || !(param_names.is_empty() || param_comptime.iter().all(|&c| c))
        {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "type constructor '{}' expects {} comptime type argument(s), \
                         but {} were provided",
                        call_name,
                        param_names.len(),
                        arg_strs.len()
                    ),
                },
                span,
            ));
        }

        // Bind each argument to its parameter by the parameter's declared
        // kind: type arguments resolve as types, value arguments as comptime
        // constants (RUE-552).
        let (callee_types, callee_values) =
            self.bind_type_ctor_args(call_name, fn_info, arg_strs, span, ctx)?;

        // Reduce the constructor body under the substitution. Shares the exact
        // reduction path (and E1200 recursion guard) with value-position calls.
        match self
            .reduce_type_ctor_body(name_key, &callee_types, &callee_values)
            .map_err(|e| Self::label_ctor_instantiation_site(e, span))?
        {
            Some(ConstValue::Type(t)) => Ok(t),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "the type constructor '{}' did not reduce to a concrete type \
                         at compile time",
                        call_name
                    ),
                },
                span,
            )),
        }
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
            .get_params(function.rir_params_start, function.rir_params_len)
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

        let rir_params = self
            .rir
            .get_params(function.rir_params_start, function.rir_params_len);
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
        let param_name = param.name;

        // Speculative comptime callers intentionally collapse errors to
        // `None`; a concrete specialized signature is authoritative. Validate
        // it first so malformed source types keep their source diagnostics.
        self.validate_substituted_signature_type(type_sym, type_subst, value_subst, function.span)?;

        self.resolve_type_for_comptime_with_subst_and_values_at_span(
            type_sym,
            type_subst,
            value_subst,
            function.span,
        )
        .ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "generic parameter '{}' still has an unresolved runtime type after specialization",
                    self.interner.resolve(&param_name)
                )),
                function.span,
            )
        })
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
        self.validate_substituted_signature_type(
            function.return_type_sym,
            type_subst,
            value_subst,
            function.span,
        )?;
        self.resolve_type_for_comptime_with_subst_and_values_at_span(
            function.return_type_sym,
            type_subst,
            value_subst,
            function.span,
        )
        .ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(
                    "generic return type remained unresolved after specialization".to_string(),
                ),
                function.span,
            )
        })
    }

    fn validate_substituted_signature_type(
        &mut self,
        type_sym: Spur,
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<()> {
        if type_subst.contains_key(&type_sym) {
            return Ok(());
        }

        let type_name = self.interner.resolve(&type_sym).to_string();
        if let Some((element, len)) = parse_array_type_syntax(&type_name) {
            let element_sym = self.interner.get_or_intern(&element);
            self.validate_substituted_signature_type(element_sym, type_subst, value_subst, span)?;
            self.resolve_array_length(&len, span, Some(value_subst))?;
        } else if let Some(pointee) = type_name
            .strip_prefix("ptr const ")
            .or_else(|| type_name.strip_prefix("ptr mut "))
        {
            let pointee_sym = self.interner.get_or_intern(pointee);
            self.validate_substituted_signature_type(pointee_sym, type_subst, value_subst, span)?;
        } else if let Some((call_name, arg_strs)) = parse_type_call_syntax(&type_name) {
            self.resolve_type_call_for_comptime_with_subst_diagnostic(
                &call_name,
                &arg_strs,
                type_subst,
                value_subst,
                span,
            )?;
        } else {
            // Resolve concrete leaves in the declaration's file. This keeps
            // same-named module-local types isolated inside deferred arrays
            // and pointers.
            self.resolve_type(type_sym, span)?;
        }
        Ok(())
    }

    /// Resolve a type-constructor application while preserving diagnostics.
    /// The general comptime resolver is intentionally Option-based because
    /// many callers probe whether an expression is reducible. Once a generic
    /// signature is concrete, malformed arity, kind, or constructor-body
    /// failures are authoritative source errors.
    fn resolve_type_call_for_comptime_with_subst_diagnostic(
        &mut self,
        call_name: &str,
        arg_strs: &[String],
        type_subst: &HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Option<Type>> {
        if call_name == "Str" {
            return self
                .resolve_str_fixed_type(call_name, arg_strs, span)
                .map(Some);
        }
        if call_name.contains('.') {
            return Ok(self.resolve_qualified_type_call_for_comptime(
                call_name,
                arg_strs,
                type_subst,
                value_subst,
                span,
            ));
        }

        let name_sym = self.interner.get_or_intern(call_name);
        let function_key = if span == Span::default() {
            Some(name_sym)
        } else {
            let mut key = self.resolve_function_name_local(name_sym, span.file_id);
            if key.is_none() && self.declaration_binding_active {
                self.collect_free_function_signature_during_binding(name_sym, Some(span.file_id))?;
                key = self.resolve_function_name_local(name_sym, span.file_id);
            }
            key
        }
        .ok_or_else(|| {
            CompileError::new(
                ErrorKind::UnknownType(format!("{}({})", call_name, arg_strs.join(", "))),
                span,
            )
        })?;
        let fn_info = self.functions.get(&function_key).copied().ok_or_else(|| {
            CompileError::new(
                ErrorKind::UnknownType(format!("{}({})", call_name, arg_strs.join(", "))),
                span,
            )
        })?;
        self.check_unqualified_visibility(
            "function",
            call_name,
            fn_info.file_id,
            fn_info.is_pub,
            span,
        )?;
        if !self.function_returns_type(&fn_info) {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "'{}' is not a type: only a function returning `type` can be applied here",
                        call_name
                    ),
                },
                span,
            ));
        }

        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        if arg_strs.len() != param_names.len()
            || !(param_names.is_empty() || param_comptime.iter().all(|&flag| flag))
        {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "type constructor '{}' expects {} comptime argument(s), but {} were provided",
                        call_name,
                        param_names.len(),
                        arg_strs.len()
                    ),
                },
                span,
            ));
        }

        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        let mut callee_types = HashMap::new();
        let mut callee_values = HashMap::new();
        for (i, arg) in arg_strs.iter().enumerate() {
            if param_comptime_type[i] {
                let arg_ty =
                    if let Some((nested_name, nested_args)) = parse_type_call_syntax(arg) {
                        self.resolve_type_call_for_comptime_with_subst_diagnostic(
                            &nested_name,
                            &nested_args,
                            type_subst,
                            value_subst,
                            span,
                        )?
                    } else {
                        let arg_sym = self.interner.get_or_intern(arg);
                        self.resolve_type_for_comptime_with_subst_and_values_at_span(
                            arg_sym,
                            type_subst,
                            value_subst,
                            span,
                        )
                    }
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "argument '{}' for '{}' did not resolve to a type",
                                    arg, call_name
                                ),
                            },
                            span,
                        )
                    })?;
                callee_types.insert(param_names[i], arg_ty);
            } else {
                let value = self
                    .resolve_comptime_type_ctor_value_arg(arg, type_subst, value_subst, span)
                    .ok_or_else(|| {
                        CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: format!(
                                    "argument '{}' for '{}' is not a compile-time value",
                                    arg, call_name
                                ),
                            },
                            span,
                        )
                    })?;
                callee_values.insert(param_names[i], value);
            }
        }

        match self
            .reduce_type_ctor_body(function_key, &callee_types, &callee_values)
            .map_err(|error| Self::label_ctor_instantiation_site(error, span))?
        {
            Some(ConstValue::Type(ty)) => Ok(Some(ty)),
            _ => Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "the type constructor '{}' did not reduce to a concrete type",
                        call_name
                    ),
                },
                span,
            )),
        }
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
        self.resolve_type_for_comptime_with_subst_and_values_at_span(
            type_sym,
            type_subst,
            value_subst,
            Span::default(),
        )
    }

    pub(crate) fn resolve_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        type_sym: Spur,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &HashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<Type> {
        // First check the substitution map for type parameters
        if let Some(&ty) = type_subst.get(&type_sym) {
            return Some(ty);
        }

        let type_name = self.interner.resolve(&type_sym);

        // Check primitive types first (single shared table, RUE-155)
        if let Some(ty) = Type::from_primitive_name(type_name) {
            return Some(ty);
        }

        let is_composite = parse_array_type_syntax(type_name).is_some()
            || type_name.starts_with("ptr const ")
            || type_name.starts_with("ptr mut ")
            || parse_type_call_syntax(type_name).is_some();
        if span != Span::default() && !is_composite {
            return self.resolve_type(type_sym, span).ok();
        }

        if let Some(struct_id) = self.struct_id_for_name(type_sym) {
            Some(Type::new_struct(struct_id))
        } else if let Some(enum_id) = self.enum_id_for_name(type_sym) {
            Some(Type::new_enum(enum_id))
        } else if let Some((element_type, len)) = parse_array_type_syntax(type_name) {
            // Resolve the element type first
            let element_sym = self.interner.get_or_intern(&element_type);
            let element_ty = self.resolve_type_for_comptime_with_subst_and_values_at_span(
                element_sym,
                type_subst,
                value_subst,
                span,
            )?;
            // Resolve the length via comptime value substitution (a `comptime`
            // value parameter) or file-level constants. In comptime evaluation
            // we can't emit a diagnostic, so an unresolvable length just makes
            // the type non-evaluable (None); the caller reports it (RUE-16).
            let length = self
                .resolve_array_length(&len, span, Some(value_subst))
                .ok()?;
            // Get or create the array type
            let array_type_id = self.get_or_create_array_type(element_ty, length);
            Some(Type::new_array(array_type_id))
        } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr const ") {
            // Pointer type syntax: ptr const T
            let pointee_sym = self.interner.get_or_intern(pointee_type_str);
            let pointee_ty = self.resolve_type_for_comptime_with_subst_and_values_at_span(
                pointee_sym,
                type_subst,
                value_subst,
                span,
            )?;
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
            Some(Type::new_ptr_const(ptr_type_id))
        } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr mut ") {
            // Pointer type syntax: ptr mut T
            let pointee_sym = self.interner.get_or_intern(pointee_type_str);
            let pointee_ty = self.resolve_type_for_comptime_with_subst_and_values_at_span(
                pointee_sym,
                type_subst,
                value_subst,
                span,
            )?;
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
            Some(Type::new_ptr_mut(ptr_type_id))
        } else if let Some((call_name, arg_strs)) = parse_type_call_syntax(type_name) {
            self.resolve_type_call_for_comptime_with_subst_diagnostic(
                &call_name,
                &arg_strs,
                type_subst,
                value_subst,
                span,
            )
            .ok()
            .flatten()
        } else {
            None // Unknown type
        }
    }

    /// Resolve an array length `[T; N]` to a concrete `u64`.
    ///
    /// `N` is either a literal or a name referring to a compile-time constant.
    /// Named lengths resolve first against the `comptime` value substitution
    /// map (a `comptime N: i32` parameter, when `value_subst` is provided), then
    /// against file-level `const`s. The value must be a non-negative integer
    /// (RUE-16). On the comptime-evaluation path the caller passes a dummy span
    /// and discards the error; on the diagnostic path (`resolve_type`) the
    /// error surfaces to the user as E0481.
    /// Reduce a fixed-capacity string `Str(N)` type-call to its concrete
    /// 2-word fat-pointer struct (ADR-0043 Phase 5, RUE-326). Shared by the
    /// context-free (`resolve_type`) and context-aware (`resolve_type_with_ctx`)
    /// annotation paths so both spellings resolve identically. A non-single-
    /// argument form (`Str()`, `Str(a, b)`) is a clean unknown-type error.
    fn resolve_str_fixed_type(
        &mut self,
        call_name: &str,
        arg_strs: &[String],
        span: Span,
    ) -> CompileResult<Type> {
        let capacity = match arg_strs {
            [arg] => self.resolve_str_fixed_capacity(arg, span)?,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::UnknownType(format!("{}({})", call_name, arg_strs.join(", "))),
                    span,
                ));
            }
        };
        self.record_declaration_builtin_type_call_head(
            super::BuiltinTypeCallHead::FixedCapacityString,
        );
        self.get_or_create_str_fixed_struct(capacity, span)
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
    fn resolve_str_fixed_capacity(&mut self, arg: &str, span: Span) -> CompileResult<u64> {
        if let Ok(n) = arg.parse::<u64>() {
            return Ok(n);
        }
        self.resolve_array_length(&ArrayLen::Named(arg.to_string()), span, None)
    }

    pub(crate) fn resolve_array_length(
        &mut self,
        len: &ArrayLen,
        span: Span,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        self.resolve_array_length_with_subst(len, span, None, value_subst)
    }

    fn resolve_array_length_with_subst(
        &mut self,
        len: &ArrayLen,
        span: Span,
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        match len {
            ArrayLen::Literal(n) => Ok(*n),
            ArrayLen::Named(name) => {
                // A comptime-evaluable call in length position, e.g.
                // `[i32; fact(4)]` (RUE-309). The interned type string carries
                // the call syntax verbatim (`fact(4)`); fold it via the same
                // comptime machinery that reduces value-returning calls
                // elsewhere (RUE-163). Bare names fall through to the
                // const/comptime-parameter lookup below.
                if let Some((callee, args)) = parse_type_call_syntax(name) {
                    return self.resolve_array_length_call(
                        &callee,
                        &args,
                        span,
                        type_subst,
                        value_subst,
                    );
                }
                let sym = self.interner.get_or_intern(name);
                // 1. A `comptime` value parameter in scope (per specialization).
                let value = if let Some(v) = value_subst.and_then(|vs| vs.get(&sym)) {
                    *v
                } else if let Some(info) = self.resolve_const_info_in_file(sym, span.file_id) {
                    // 2. A file-level constant, evaluated during declaration
                    //    gathering.
                    info.value
                } else if self.declaration_binding_active
                    && let Some(v) =
                        self.try_resolve_indexed_const_during_binding(sym, span.file_id)
                {
                    // 3. A file-level constant whose indexed declaration is
                    //    being dependency-resolved for a struct field / enum
                    //    payload before the main declaration walk (RUE-587).
                    v
                } else {
                    return Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!(
                                "'{name}' is not a compile-time constant; array lengths must be an \
                                 integer literal, a `const`, or a `comptime` value parameter"
                            ),
                        },
                        span,
                    ));
                };
                match value.as_int_value() {
                    Some(n) if n >= 0 => u64::try_from(n).map_err(|_| {
                        CompileError::new(
                            ErrorKind::InvalidArrayLength {
                                reason: format!("array length '{name}' ({n}) is too large"),
                            },
                            span,
                        )
                    }),
                    Some(n) => Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!("array length '{name}' is negative ({n})"),
                        },
                        span,
                    )),
                    None => Err(CompileError::new(
                        ErrorKind::InvalidArrayLength {
                            reason: format!("array length '{name}' is not an integer"),
                        },
                        span,
                    )),
                }
            }
        }
    }

    /// Fold a comptime-evaluable call in array-length position (`[T; f(args)]`,
    /// RUE-309) to a concrete `u64`.
    ///
    /// The callee must be a value-returning function with at least one
    /// parameter, all `comptime` — the same implicit-comptime shape
    /// `eval_comptime_type_call` accepts for value-returning calls (RUE-163,
    /// spec 4.14:5). A runtime-parametered or nullary callee is a genuine
    /// runtime call and is not a compile-time-known length, so it errors as
    /// E0481 rather than being silently accepted.
    ///
    /// Arguments arrive as raw substrings of the interned type string; each is
    /// itself an array-length expression (a literal, a `const`/`comptime`
    /// name, or a nested call), so it is resolved through
    /// [`resolve_array_length`] recursively. The resulting integer bindings
    /// feed [`reduce_type_ctor_body`], which evaluates the callee body under
    /// that substitution — the identical reducer the value/const-expr paths
    /// use — so a comptime-recursive `fn fact(comptime n: i32)` yields its
    /// compile-time length.
    ///
    /// [`resolve_array_length`]: Sema::resolve_array_length
    /// [`reduce_type_ctor_body`]: Sema::reduce_type_ctor_body
    fn resolve_array_length_call(
        &mut self,
        callee: &str,
        args: &[String],
        span: Span,
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        let invalid =
            |reason: String| CompileError::new(ErrorKind::InvalidArrayLength { reason }, span);

        let callee_sym = self.interner.get_or_intern(callee);
        let Some(callee_key) = self.resolve_function_name_local(callee_sym, span.file_id) else {
            return Err(invalid(format!(
                "'{callee}' is not a function; array lengths must be an integer literal, a \
                 `const`, a `comptime` value parameter, or a call to a comptime function"
            )));
        };
        let Some(fn_info) = self.functions.get(&callee_key).copied() else {
            return Err(invalid(format!(
                "'{callee}' is not a function; array lengths must be an integer literal, a \
                 `const`, a `comptime` value parameter, or a call to a comptime function"
            )));
        };
        if self.function_returns_type(&fn_info) {
            return Err(invalid(format!(
                "array length call '{callee}(...)' must return a value, not a type"
            )));
        }
        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        let param_comptime_type = self.comptime_type_param_flags(&fn_info);
        // Same implicit-comptime gate as `eval_comptime_type_call`: a value
        // function reduces only with at least one parameter, every one
        // `comptime`. A runtime parameter makes this a genuine runtime call.
        let all_comptime = !param_names.is_empty() && param_comptime.iter().all(|&c| c);
        if args.len() != param_names.len() || !all_comptime {
            return Err(invalid(format!(
                "array length call '{callee}(...)' is not a compile-time constant; its callee \
                 must be a value-returning function whose parameters are all `comptime`"
            )));
        }
        let empty_type_subst = HashMap::new();
        let type_subst = type_subst.unwrap_or(&empty_type_subst);
        let empty_value_subst = HashMap::new();
        let value_subst = value_subst.unwrap_or(&empty_value_subst);
        let mut callee_types: HashMap<Spur, Type> = HashMap::new();
        let mut callee_values: HashMap<Spur, ConstValue> = HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            if param_comptime_type[i] {
                let arg_sym = self.interner.get_or_intern(arg.trim());
                self.validate_substituted_signature_type(arg_sym, type_subst, value_subst, span)?;
                let Some(ty) = self.resolve_type_for_comptime_with_subst_and_values_at_span(
                    arg_sym,
                    type_subst,
                    value_subst,
                    span,
                ) else {
                    return Err(invalid(format!(
                        "argument '{}' for comptime type parameter '{}' of array length call '{}(...)' must be a type",
                        arg,
                        self.interner.resolve(&param_names[i]),
                        callee
                    )));
                };
                callee_types.insert(param_names[i], ty);
                continue;
            }

            if let Some(value) =
                self.resolve_comptime_type_ctor_value_arg(arg, type_subst, value_subst, span)
            {
                callee_values.insert(param_names[i], value);
                continue;
            }

            // Mirror `parse_array_type_syntax`: a decimal literal is a
            // `Literal`, anything else (a name or nested call) is a `Named`
            // resolved recursively.
            let arg_len = match arg.parse::<u64>() {
                Ok(n) => ArrayLen::Literal(n),
                Err(_) => ArrayLen::Named(arg.clone()),
            };
            let v = self.resolve_array_length_with_subst(
                &arg_len,
                span,
                Some(type_subst),
                Some(value_subst),
            )?;
            callee_values.insert(param_names[i], ConstValue::Integer(v as i128));
        }
        match self.reduce_type_ctor_body(callee_key, &callee_types, &callee_values)? {
            Some(ConstValue::Integer(n)) if n >= 0 => u64::try_from(n)
                .map_err(|_| invalid(format!("array length '{callee}(...)' ({n}) is too large"))),
            Some(ConstValue::Integer(n)) => Err(invalid(format!(
                "array length '{callee}(...)' is negative ({n})"
            ))),
            _ => Err(invalid(format!(
                "array length call '{callee}(...)' did not evaluate to a compile-time integer"
            ))),
        }
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

    /// Resolve a compile-time callee while validating a deferred signature.
    ///
    /// The lookup is deliberately independent of the callee's return kind:
    /// type position requires `-> type`, while array lengths and nested value
    /// arguments require an ordinary value. Keeping one lookup path lets the
    /// position-aware walker make that distinction after it has followed the
    /// source kinds of the callee's parameters.
    fn deferred_comptime_function_info(
        &mut self,
        call_name: &str,
        span: Span,
    ) -> CompileResult<(Spur, FunctionInfo)> {
        let declaration_type_observer = self.declaration_type_observer.clone();
        let unknown =
            || CompileError::new(ErrorKind::UnknownType(format!("{}(...)", call_name)), span);

        let function_key = if call_name.contains('.') {
            let segments: Vec<&str> = call_name.split('.').collect();
            if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
                return Err(unknown());
            }
            let (_module_id, module_file_id, _module_path) =
                self.resolve_type_module_prefix(&segments[..segments.len() - 1], span)?;
            let module_file_id = module_file_id.ok_or_else(unknown)?;
            let member = segments[segments.len() - 1];
            let member_sym = self.interner.get_or_intern(member);
            if self.declaration_binding_active {
                self.collect_free_function_signature_during_binding(
                    member_sym,
                    Some(module_file_id),
                )?;
            }
            let key = self
                .resolve_function_name_local(member_sym, module_file_id)
                .ok_or_else(unknown)?;
            let info = self.functions.get(&key).copied().ok_or_else(unknown)?;
            if info.file_id != module_file_id {
                return Err(unknown());
            }
            self.check_unqualified_visibility("function", member, info.file_id, info.is_pub, span)?;
            key
        } else {
            let name_sym = self.interner.get_or_intern(call_name);
            let mut key = self.resolve_function_name_local(name_sym, span.file_id);
            if key.is_none() && self.declaration_binding_active {
                self.collect_free_function_signature_during_binding(name_sym, Some(span.file_id))?;
                key = self.resolve_function_name_local(name_sym, span.file_id);
            }
            let key = key.ok_or_else(unknown)?;
            let info = self.functions.get(&key).copied().ok_or_else(unknown)?;
            self.check_unqualified_visibility(
                "function",
                call_name,
                info.file_id,
                info.is_pub,
                span,
            )?;
            key
        };

        let info = self
            .functions
            .get(&function_key)
            .copied()
            .ok_or_else(unknown)?;
        self.declaration_type_observer = declaration_type_observer;
        self.record_declaration_type_call_head(function_key, info);
        Ok((function_key, info))
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
        if let Some((element_type, len)) = parse_array_type_syntax(&type_name) {
            if let ArrayLen::Named(name) = &len {
                self.validate_deferred_value_position(
                    name,
                    type_params,
                    value_params,
                    value_param_type_syms,
                    None,
                    None,
                    true,
                    span,
                )?;
                let depends_on_param = self.type_name_mentions_type_param(name, type_params)
                    || self.type_name_mentions_value_param(name, value_params);
                if !depends_on_param {
                    self.resolve_array_length(&len, span, None)?;
                }
            }
            self.validate_deferred_type_position(
                element_type,
                type_params,
                value_params,
                value_param_type_syms,
                span,
            )?;
            return self.resolve_independent_deferred_type(
                &type_name,
                type_params,
                value_params,
                span,
            );
        }

        if let Some(pointee) = type_name
            .strip_prefix("ptr const ")
            .or_else(|| type_name.strip_prefix("ptr mut "))
        {
            self.validate_deferred_type_position(
                pointee.to_string(),
                type_params,
                value_params,
                value_param_type_syms,
                span,
            )?;
            return self.resolve_independent_deferred_type(
                &type_name,
                type_params,
                value_params,
                span,
            );
        }

        if let Some((call_name, arg_strs)) = parse_type_call_syntax(&type_name) {
            let (function_key, function) =
                self.deferred_comptime_function_info(&call_name, span)?;
            if !self.function_returns_type(&function) {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "'{}' is not a type: only a function returning `type` can be applied here",
                            call_name
                        ),
                    },
                    span,
                ));
            }

            let (callee_types, callee_values) = self.validate_deferred_comptime_call_args(
                &call_name,
                function,
                &arg_strs,
                type_params,
                value_params,
                value_param_type_syms,
                span,
            )?;
            if callee_types.len() + callee_values.len() == arg_strs.len() {
                return match self.reduce_type_ctor_body(
                    function_key,
                    &callee_types,
                    &callee_values,
                )? {
                    Some(ConstValue::Type(ty)) => Ok(Some(ty)),
                    _ => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason: format!(
                                "the type constructor '{}' did not reduce to a concrete type",
                                call_name
                            ),
                        },
                        span,
                    )),
                };
            }
            return Ok(None);
        }

        // Only a source `comptime T: type` parameter may stand in type
        // position. A value parameter that happens to share the semantic
        // `type` placeholder is still a value, and accepting it here would
        // defer an intrinsically malformed signature forever.
        if let Some(sym) = self.interner.get(&type_name) {
            if type_params.contains(&sym) {
                return Ok(None);
            }
            if value_params.contains(&sym) {
                return Err(CompileError::new(ErrorKind::UnknownType(type_name), span));
            }
        }

        let sym = self.interner.get_or_intern(&type_name);
        self.resolve_type(sym, span).map(Some)
    }

    fn resolve_independent_deferred_type(
        &mut self,
        type_name: &str,
        type_params: &[Spur],
        value_params: &[Spur],
        span: Span,
    ) -> CompileResult<Option<Type>> {
        if self.type_name_mentions_type_param(type_name, type_params)
            || self.type_name_mentions_value_param(type_name, value_params)
        {
            return Ok(None);
        }
        let sym = self.interner.get_or_intern(type_name);
        self.resolve_type(sym, span).map(Some)
    }

    /// Validate and partially bind a nested compile-time call. Argument
    /// positions follow the callee's source declaration: type parameters walk
    /// the type grammar, while value parameters are checked against their
    /// declared type after applying all preceding concrete substitutions.
    fn validate_deferred_comptime_call_args(
        &mut self,
        call_name: &str,
        function: FunctionInfo,
        args: &[String],
        outer_type_params: &[Spur],
        outer_value_params: &[Spur],
        outer_value_param_type_syms: &[(Spur, Spur)],
        span: Span,
    ) -> CompileResult<(HashMap<Spur, Type>, HashMap<Spur, ConstValue>)> {
        let params = function.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_types = self.param_arena.types(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        if args.len() != param_names.len()
            || !(param_names.is_empty() || param_comptime.iter().all(|&flag| flag))
        {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "compile-time function '{}' expects {} comptime argument(s), but {} were provided",
                        call_name,
                        param_names.len(),
                        args.len()
                    ),
                },
                span,
            ));
        }

        let param_comptime_type = self.comptime_type_param_flags(&function);
        let rir_param_types: Vec<Spur> = self
            .rir
            .get_params(function.rir_params_start, function.rir_params_len)
            .iter()
            .map(|param| param.ty)
            .collect();
        let callee_type_params: Vec<Spur> = param_names
            .iter()
            .zip(param_comptime_type.iter())
            .filter_map(|(name, is_type)| is_type.then_some(*name))
            .collect();
        let callee_value_params: Vec<Spur> = param_names
            .iter()
            .zip(param_comptime_type.iter())
            .filter_map(|(name, is_type)| (!is_type).then_some(*name))
            .collect();
        let call_display = self.interner.get_or_intern(call_name);
        let mut callee_types = HashMap::new();
        let mut callee_values = HashMap::new();

        for (index, arg) in args.iter().enumerate() {
            if param_comptime_type[index] {
                if let Some(ty) = self.validate_deferred_type_position(
                    arg.clone(),
                    outer_type_params,
                    outer_value_params,
                    outer_value_param_type_syms,
                    span,
                )? {
                    callee_types.insert(param_names[index], ty);
                }
                continue;
            }

            let expected = if param_types[index] != Type::COMPTIME_TYPE {
                Some(param_types[index])
            } else if let Some(bound) = callee_types.get(&rir_param_types[index]) {
                Some(*bound)
            } else if self.deferred_signature_substitutions_are_ready(
                self.interner.resolve(&rir_param_types[index]),
                &callee_type_params,
                &callee_value_params,
                &callee_types,
                &callee_values,
            ) {
                Some(self.resolve_substituted_param_type(
                    &function,
                    index,
                    param_types[index],
                    &callee_types,
                    &callee_values,
                )?)
            } else {
                None
            };

            let value = self.validate_deferred_value_position(
                arg,
                outer_type_params,
                outer_value_params,
                outer_value_param_type_syms,
                expected,
                Some((call_display, param_names[index])),
                false,
                span,
            )?;
            if let Some(value) = value {
                callee_values.insert(param_names[index], value);
            }
        }

        Ok((callee_types, callee_values))
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
            let (function_key, function) =
                self.deferred_comptime_function_info(&call_name, span)?;
            let param_names = self.param_arena.names(function.params).to_vec();
            let param_comptime = self.param_arena.comptime(function.params).to_vec();
            let is_type_function = self.function_returns_type(&function);
            let eligible = if is_type_function {
                param_names.is_empty() || param_comptime.iter().all(|&flag| flag)
            } else {
                !param_names.is_empty() && param_comptime.iter().all(|&flag| flag)
            };
            if !eligible {
                return Err(CompileError::new(
                    ErrorKind::ComptimeEvaluationFailed {
                        reason: format!(
                            "call '{}' is not a compile-time value; all of its parameters must be comptime",
                            call_name
                        ),
                    },
                    span,
                ));
            }

            let (callee_types, callee_values) = self.validate_deferred_comptime_call_args(
                &call_name,
                function,
                &args,
                type_params,
                value_params,
                value_param_type_syms,
                span,
            )?;
            let fully_bound = callee_types.len() + callee_values.len() == args.len();
            let concrete = if fully_bound {
                self.reduce_type_ctor_body(function_key, &callee_types, &callee_values)?
            } else {
                None
            };
            let found = if is_type_function {
                Some(Type::COMPTIME_TYPE)
            } else if function.return_type != Type::COMPTIME_TYPE {
                Some(function.return_type)
            } else {
                let param_comptime_type = self.comptime_type_param_flags(&function);
                let callee_type_params: Vec<Spur> = param_names
                    .iter()
                    .zip(param_comptime_type.iter())
                    .filter_map(|(name, is_type)| is_type.then_some(*name))
                    .collect();
                let callee_value_params: Vec<Spur> = param_names
                    .iter()
                    .zip(param_comptime_type.iter())
                    .filter_map(|(name, is_type)| (!is_type).then_some(*name))
                    .collect();
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

        let value =
            match self.resolve_type_ctor_value_arg("compile-time call", value_name, span, None) {
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
        self.type_pool.abi_slot_count(ty)
    }
}
