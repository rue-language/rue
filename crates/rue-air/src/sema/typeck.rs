//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use std::collections::HashMap;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::Span;

use super::Sema;
use super::context::AnalysisContext;
use crate::inference::InferType;
use crate::sema::ConstValue;
use crate::types::{
    ArrayLen, ArrayTypeId, Type, TypeKind, parse_array_type_syntax, parse_type_call_syntax,
};

impl<'a> Sema<'a> {
    /// Get a human-readable name for a type.
    pub(crate) fn format_type_name(&self, ty: Type) -> String {
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
            // Note: String is now handled via TypeKind::Struct with builtin_string_id
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
    pub(crate) fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let type_name = self.interner.resolve(&type_sym);

        // Check primitive types first (single shared table, RUE-155).
        // Note: String is handled below via struct lookup (it's a builtin struct).
        if let Some(ty) = Type::from_primitive_name(type_name) {
            return Ok(ty);
        }

        if let Some(&struct_id) = self.structs.get(&type_sym) {
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
        } else if let Some(&enum_id) = self.enums.get(&type_sym) {
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
                // A type-function application written directly in type position
                // (`Result(i32, i32)`; RUE-241). Reduce the comptime type call
                // to its monomorphized concrete type. No analysis context is
                // available on this context-free path, so arguments resolve
                // context-free (a signature/return position collected before any
                // body context exists).
                self.resolve_type_function_call(&call_name, &arg_strs, span, None)
            } else {
                Err(CompileError::new(
                    ErrorKind::UnknownType(type_name.to_string()),
                    span,
                ))
            }
        }
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
    /// Array lengths are resolved context-free (literals and file-level
    /// `const`s), exactly as [`resolve_type`] does — a length naming a local
    /// `comptime N: i32` parameter (`[P; N]`) is *not* resolved here and gets
    /// the same E0481 it does today. Threading the comptime value map so `N`
    /// resolves would surface a latent rue-cfg drop-analysis ICE for
    /// comptime-value-length *local* arrays (the length only works in signature
    /// and return positions so far, RUE-252); that gap is tracked separately.
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
            let length = self.resolve_array_length(&len, span, None)?;
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
    fn resolve_type_function_call(
        &mut self,
        call_name: &str,
        arg_strs: &[String],
        span: Span,
        ctx: Option<&AnalysisContext>,
    ) -> CompileResult<Type> {
        let name_sym = self.interner.get_or_intern(call_name);

        // The callee must be a known `-> type` constructor.
        let Some(fn_info) = self.functions.get(&name_sym) else {
            return Err(CompileError::new(
                ErrorKind::UnknownType(format!("{}(...)", call_name)),
                span,
            ));
        };
        let is_type_ctor = fn_info.return_type == Type::COMPTIME_TYPE;
        let params = fn_info.params;
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

        // Resolve each argument as a type and bind it to the corresponding
        // comptime type parameter.
        let mut callee_types: HashMap<Spur, Type> = HashMap::new();
        for (i, arg) in arg_strs.iter().enumerate() {
            let arg_sym = self.interner.get_or_intern(arg);
            let arg_ty = match ctx {
                Some(ctx) => self.resolve_type_with_ctx(arg_sym, span, ctx)?,
                None => self.resolve_type(arg_sym, span)?,
            };
            callee_types.insert(param_names[i], arg_ty);
        }

        // Reduce the constructor body under the substitution. Shares the exact
        // reduction path (and E1200 recursion guard) with value-position calls.
        let empty_values: HashMap<Spur, ConstValue> = HashMap::new();
        match self.reduce_type_ctor_body(name_sym, &callee_types, &empty_values)? {
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
        // First check the substitution map for type parameters
        if let Some(&ty) = type_subst.get(&type_sym) {
            return Some(ty);
        }

        let type_name = self.interner.resolve(&type_sym);

        // Check primitive types first (single shared table, RUE-155)
        if let Some(ty) = Type::from_primitive_name(type_name) {
            return Some(ty);
        }

        if let Some(&struct_id) = self.structs.get(&type_sym) {
            Some(Type::new_struct(struct_id))
        } else if let Some(&enum_id) = self.enums.get(&type_sym) {
            Some(Type::new_enum(enum_id))
        } else if let Some((element_type, len)) = parse_array_type_syntax(type_name) {
            // Resolve the element type first
            let element_sym = self.interner.get_or_intern(&element_type);
            let element_ty = self.resolve_type_for_comptime_with_subst_and_values(
                element_sym,
                type_subst,
                value_subst,
            )?;
            // Resolve the length via comptime value substitution (a `comptime`
            // value parameter) or file-level constants. In comptime evaluation
            // we can't emit a diagnostic, so an unresolvable length just makes
            // the type non-evaluable (None); the caller reports it (RUE-16).
            let length = self
                .resolve_array_length(&len, Span::default(), Some(value_subst))
                .ok()?;
            // Get or create the array type
            let array_type_id = self.get_or_create_array_type(element_ty, length);
            Some(Type::new_array(array_type_id))
        } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr const ") {
            // Pointer type syntax: ptr const T
            let pointee_sym = self.interner.get_or_intern(pointee_type_str);
            let pointee_ty = self.resolve_type_for_comptime_with_subst_and_values(
                pointee_sym,
                type_subst,
                value_subst,
            )?;
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_ty);
            Some(Type::new_ptr_const(ptr_type_id))
        } else if let Some(pointee_type_str) = type_name.strip_prefix("ptr mut ") {
            // Pointer type syntax: ptr mut T
            let pointee_sym = self.interner.get_or_intern(pointee_type_str);
            let pointee_ty = self.resolve_type_for_comptime_with_subst_and_values(
                pointee_sym,
                type_subst,
                value_subst,
            )?;
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_ty);
            Some(Type::new_ptr_mut(ptr_type_id))
        } else if let Some((call_name, arg_strs)) = parse_type_call_syntax(type_name) {
            // A type-function application whose arguments may name enclosing
            // comptime type parameters (`Option(T)` with `T` in `type_subst`).
            // Resolve each argument under the current substitution, then reduce
            // the constructor body to its monomorphized type — so a generic
            // signature/return type applying an enclosing constructor to a type
            // parameter (`fn wrap(comptime T: type, ...) -> Option(T)`)
            // monomorphizes at each call site (RUE-272). Shares the reduction
            // path (and E1200 recursion guard) with the signature-position
            // resolver `resolve_type_function_call`. On the comptime path we
            // can't emit a diagnostic, so any failure (unknown callee,
            // non-`type` callee, arity mismatch, non-reducing body, recursion
            // guard) just makes the type non-evaluable (`None`); the caller
            // reports it.
            let name_sym = self.interner.get_or_intern(&call_name);
            let fn_info = self.functions.get(&name_sym)?;
            if fn_info.return_type != Type::COMPTIME_TYPE {
                return None;
            }
            let params = fn_info.params;
            let param_names = self.param_arena.names(params).to_vec();
            let param_comptime = self.param_arena.comptime(params).to_vec();
            if arg_strs.len() != param_names.len()
                || !(param_names.is_empty() || param_comptime.iter().all(|&c| c))
            {
                return None;
            }
            let mut callee_types: HashMap<Spur, Type> = HashMap::new();
            for (i, arg) in arg_strs.iter().enumerate() {
                let arg_sym = self.interner.get_or_intern(arg);
                let arg_ty = self.resolve_type_for_comptime_with_subst_and_values(
                    arg_sym,
                    type_subst,
                    value_subst,
                )?;
                callee_types.insert(param_names[i], arg_ty);
            }
            let empty_values: HashMap<Spur, ConstValue> = HashMap::new();
            match self
                .reduce_type_ctor_body(name_sym, &callee_types, &empty_values)
                .ok()?
            {
                Some(ConstValue::Type(t)) => Some(t),
                _ => None,
            }
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
    pub(crate) fn resolve_array_length(
        &mut self,
        len: &ArrayLen,
        span: Span,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        match len {
            ArrayLen::Literal(n) => Ok(*n),
            ArrayLen::Named(name) => {
                let sym = self.interner.get_or_intern(name);
                // 1. A `comptime` value parameter in scope (per specialization).
                let value = if let Some(v) = value_subst.and_then(|vs| vs.get(&sym)) {
                    *v
                } else if let Some(info) = self.constants.get(&sym) {
                    // 2. A file-level constant, evaluated during declaration
                    //    gathering.
                    info.value
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
        if let Some((element_type, _length)) = parse_array_type_syntax(type_name) {
            return self.type_name_mentions_type_param(&element_type, type_params);
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
            // The length itself may name a value parameter.
            if let ArrayLen::Named(name) = &len {
                if let Some(sym) = self.interner.get(name) {
                    if value_params.contains(&sym) {
                        return true;
                    }
                }
            }
            // Recurse into the element type (nested arrays / pointers).
            return self.type_name_mentions_value_param(&element_type, value_params);
        }
        if let Some(pointee) = type_name
            .strip_prefix("ptr const ")
            .or_else(|| type_name.strip_prefix("ptr mut "))
        {
            return self.type_name_mentions_value_param(pointee, value_params);
        }
        false
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

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot,
    /// structs use 1 slot per field, arrays use 1 slot per element.
    /// Zero-sized types (unit, never, empty structs, zero-length arrays) use 0 slots.
    pub(crate) fn abi_slot_count(&self, ty: Type) -> u32 {
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
            | TypeKind::Error => 1,
            // Zero-sized types use 0 slots
            // ComptimeType is comptime-only and uses 0 runtime slots
            TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType => 0,
            // Tagged-union layout (RUE-221, ADR-0038): slot 0 is the
            // discriminant, followed by payload space sized to the largest
            // variant. A discriminant-only (C-like) enum has no payload and so
            // occupies exactly one slot. This MUST match the codegen layout in
            // `rue_codegen::types::type_slot_count`.
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
                let mut max_payload = 0u32;
                for i in 0..enum_def.variant_count() {
                    let variant_slots: u32 = enum_def
                        .variant_payload(i)
                        .iter()
                        .map(|&ty| self.abi_slot_count(ty))
                        .sum();
                    max_payload = max_payload.max(variant_slots);
                }
                1 + max_payload
            }
            // Struct uses sum of all field slots (includes builtin String with 3 fields)
            TypeKind::Struct(struct_id) => {
                // Sum the slot counts of all fields (handles arrays, nested structs, and builtins)
                // Empty structs naturally get 0 slots here
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def
                    .fields
                    .iter()
                    .map(|f| self.abi_slot_count(f.ty))
                    .sum()
            }
            TypeKind::Array(array_type_id) => {
                // Zero-length arrays naturally get 0 slots (0 * element_slots)
                let (element_type, length) = self.type_pool.array_def(array_type_id);
                let element_slots = self.abi_slot_count(element_type);
                element_slots * length as u32
            }
            // Module types don't take ABI slots (they're compile-time only)
            TypeKind::Module(_) => 0,
            // Pointer types take 1 slot (64-bit address)
            TypeKind::PtrConst(_) | TypeKind::PtrMut(_) => 1,
        }
    }
}
