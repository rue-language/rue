//! Canonical AST-to-RIR generation.
//!
//! [`AstGen`] converts canonical parsed module items into one program-wide RIR
//! instruction space. It is analogous to Zig's AstGen phase, but intentionally
//! has no per-AST construction path: callers normalize symbols, append module
//! items in order, and finish one lowering session.

use std::collections::{HashMap, HashSet};

use lasso::{Spur, ThreadedRodeo};

use rue_parser::ast::{ConstDecl, DropFn, ExternBlock, ExternFn};
use rue_parser::intrinsics::{OFFSET_OF_INTRINSIC, TYPE_INTRINSICS};
use rue_parser::{
    ArgMode, ArrayLength, AssignTarget, BinaryOp, CallArg, Directive, DirectiveArg, EnumDecl, Expr,
    Function, IntrinsicArg, Item, LetPattern, Method, ParamMode, Pattern, Statement, StructDecl,
    TypeExpr, UnaryOp, ast::Visibility,
};

use crate::inst::{
    Inst, InstData, InstRef, InternalIntrinsic, PayloadFallback, RepeatCount, Rir, RirArgMode,
    RirCallArg, RirDirective, RirEditor, RirParam, RirParamMode, RirPattern,
};

trait RecordPayloadFailure<T> {
    fn record_failure(self, first_error: &mut Option<crate::RirPayloadBuildError>) -> T;
}

impl<T: PayloadFallback> RecordPayloadFailure<T> for Result<T, crate::RirPayloadBuildError> {
    fn record_failure(self, first_error: &mut Option<crate::RirPayloadBuildError>) -> T {
        match self {
            Ok(value) => value,
            Err(error) => {
                if first_error.is_none() {
                    *first_error = Some(error);
                }
                T::payload_fallback()
            }
        }
    }
}

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// String interner for symbols (thread-safe, takes shared reference)
    interner: &'a ThreadedRodeo,
    /// Output RIR
    rir: RirEditor,
    payload_error: Option<crate::RirPayloadBuildError>,
    /// Monotonic counter used to mint unique names for the compiler-generated
    /// temporaries of a `for`-loop desugaring (RUE-220), so nested for-loops
    /// don't shadow one another's position/length/collection bindings.
    for_counter: u32,
    /// Typed structural route to the AST node currently being lowered. The
    /// route is relative to its producing definition and contains no spans or
    /// global instruction ordinals. Still the source of string-literal and
    /// read-only-data anchors; anonymous-type anchors no longer derive from it.
    structural_path: Vec<crate::RirStructuralPathSegment>,
    /// The single anonymous-type anchor authority (RUE-1089, Theme 1). Each
    /// value-position anonymous type literal reachable while reducing a producer
    /// body is recorded here by exact source span when that producer root is
    /// entered, carrying the anchor the shared [`crate::anonymous_type_sites`]
    /// walk mints. `AstGen` looks each anonymous literal up here instead of
    /// minting a second, drift-prone anchor from its own `structural_path`; a
    /// missing or kind-mismatched lookup fails closed as an internal error.
    anonymous_anchors:
        HashMap<rue_span::Span, (crate::AnonymousTypeSiteKind, crate::RirStructuralAnchor)>,
    authoritative_anonymous_anchors: bool,
    /// Nesting depth of semantic producer roots. A transported authoritative
    /// table belongs to the outer exact declaration only; method bodies nested
    /// inside an anonymous type are independent producers and obtain their
    /// anchors from the shared frontend walk when their root is entered.
    producer_root_depth: usize,
    normalize_symbol: Box<dyn Fn(Spur) -> Spur + 'a>,
}

impl<'a> AstGen<'a> {
    /// Create a generator whose AST-origin symbols are normalized before use.
    #[doc(hidden)]
    pub fn with_symbol_normalizer(
        interner: &'a ThreadedRodeo,
        normalize_symbol: impl Fn(Spur) -> Spur + 'a,
    ) -> Self {
        Self {
            interner,
            rir: RirEditor::new(),
            payload_error: None,
            for_counter: 0,
            structural_path: Vec::new(),
            anonymous_anchors: HashMap::new(),
            authoritative_anonymous_anchors: false,
            producer_root_depth: 0,
            normalize_symbol: Box::new(normalize_symbol),
        }
    }

    /// Install the exact anonymous-type anchors transported for one complete
    /// producer declaration. When installed, this table is the sole authority
    /// for anonymous type identity: producer-root lowering does not derive a
    /// second table from the AST walk, and every supplied entry must be
    /// consumed by exactly one matching literal.
    pub fn install_authoritative_anonymous_anchors(
        &mut self,
        anchors: impl IntoIterator<
            Item = (
                rue_span::Span,
                crate::AnonymousTypeSiteKind,
                crate::RirStructuralAnchor,
            ),
        >,
    ) -> Result<(), crate::RirPayloadBuildError> {
        self.authoritative_anonymous_anchors = true;
        let mut seen_anchors = HashSet::new();
        for (span, kind, anchor) in anchors {
            if self.anonymous_anchors.contains_key(&span) || !seen_anchors.insert(anchor.clone()) {
                return Err(crate::RirPayloadBuildError::InvalidBuilderInput {
                    family: "anonymous type anchor",
                    reason: "authoritative table aliases a source locator or anchor",
                });
            }
            self.anonymous_anchors.insert(span, (kind, anchor));
        }
        Ok(())
    }

    /// Append borrowed items while preserving generator-global state.
    #[doc(hidden)]
    pub fn append_items<'item>(&mut self, items: impl IntoIterator<Item = &'item Item>) {
        for item in items {
            self.gen_item(item);
        }
    }

    /// Current instruction count for read-only multi-module presentation metadata.
    #[doc(hidden)]
    pub fn instruction_len(&self) -> usize {
        self.rir.len()
    }

    /// Current payload word count for read-only multi-module presentation metadata.
    #[doc(hidden)]
    pub fn extra_len(&self) -> usize {
        self.rir.extra_len()
    }

    /// Finish a normalized multi-module lowering session.
    #[doc(hidden)]
    pub fn finish(self) -> Rir {
        self.try_finish()
            .expect("AstGen payload construction failed")
    }

    /// Finish while retaining the owner-mediated editor for controlled test
    /// synthesis and compiler-internal replacement operations.
    #[doc(hidden)]
    pub fn finish_editor(self) -> RirEditor {
        self.try_finish_editor()
            .expect("AstGen payload construction failed")
    }

    /// Finish while preserving categorized resource/capacity/builder failures.
    pub fn try_finish(self) -> Result<Rir, crate::RirPayloadBuildError> {
        Ok(self.try_finish_editor()?.into_unvalidated())
    }

    /// Finish into the owner-mediated construction form used by the canonical
    /// validation/publication boundary.
    pub fn try_finish_editor(self) -> Result<RirEditor, crate::RirPayloadBuildError> {
        if let Some(error) = self.payload_error {
            return Err(error);
        }
        if self.authoritative_anonymous_anchors && !self.anonymous_anchors.is_empty() {
            return Err(crate::RirPayloadBuildError::InvalidBuilderInput {
                family: "anonymous type anchor",
                reason: "authoritative table contains an unconsumed source locator",
            });
        }
        self.rir
            .validate_payloads()
            .expect("AstGen produced malformed RIR payloads");
        Ok(self.rir)
    }

    fn symbol(&self, symbol: Spur) -> Spur {
        (self.normalize_symbol)(symbol)
    }

    fn with_structural_segment<T>(
        &mut self,
        segment: crate::RirStructuralPathSegment,
        action: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.structural_path.push(segment);
        let result = action(self);
        self.structural_path.pop();
        result
    }

    /// Run one semantic producer with a producer-relative structural path,
    /// recording every value-position anonymous type literal reachable while
    /// reducing `root` with the anchor the shared [`crate::anonymous_type_sites`]
    /// walk mints for it.
    ///
    /// A method may be discovered while traversing a containing struct, for
    /// example, but its body is an independent semantic producer. Anchors in
    /// that body must therefore be unaffected by sibling member insertion or
    /// reordering in the owner. The walk is the single anchor authority: it does
    /// not descend into an anonymous struct's method bodies, so each such method
    /// is entered here as its own root and contributes its own sites. Spans are
    /// globally unique, so the accumulated map needs no per-root reset.
    fn with_producer_root<T>(&mut self, root: &Expr, action: impl FnOnce(&mut Self) -> T) -> T {
        let outer_path = std::mem::take(&mut self.structural_path);
        let has_transported_root =
            self.authoritative_anonymous_anchors && self.producer_root_depth == 0;
        if !has_transported_root {
            for site in crate::anonymous_type_sites(root) {
                self.anonymous_anchors
                    .insert(site.span, (site.kind, site.anchor));
            }
        }
        self.producer_root_depth += 1;
        let result = action(self);
        self.producer_root_depth -= 1;
        self.structural_path = outer_path;
        result
    }

    /// Run one body-less semantic producer (an `extern` foreign function) with a
    /// producer-relative structural path. Such a producer has no body to reduce
    /// and therefore no value-position anonymous type literals.
    fn with_bodyless_producer_root<T>(&mut self, action: impl FnOnce(&mut Self) -> T) -> T {
        let outer_path = std::mem::take(&mut self.structural_path);
        let result = action(self);
        self.structural_path = outer_path;
        result
    }

    fn gen_expr_at(&mut self, segment: crate::RirStructuralPathSegment, expr: &Expr) -> InstRef {
        self.with_structural_segment(segment, |this| this.gen_expr(expr))
    }

    fn intern_type_at(&mut self, segment: crate::RirStructuralPathSegment, ty: &TypeExpr) -> Spur {
        self.with_structural_segment(segment, |this| this.intern_type(ty))
    }

    /// The exact frontend anchor for the anonymous type literal at `span`, from
    /// the single-authority table populated when its producer root was entered
    /// (RUE-1089, Theme 1). A missing locator or a kind disagreement is an
    /// invariant violation between the walk and this lowering; it fails closed as
    /// a typed internal error (no recompute, no fallback) rather than mint a
    /// silent second anchor.
    fn anonymous_type_anchor(
        &mut self,
        span: rue_span::Span,
        kind: crate::AnonymousTypeSiteKind,
    ) -> crate::RirStructuralAnchor {
        match self.anonymous_anchors.remove(&span) {
            Some((site_kind, anchor)) if site_kind == kind => anchor,
            Some(_) => {
                self.record_anonymous_anchor_failure(
                    "anonymous type literal kind disagrees with its transported frontend anchor",
                );
                crate::RirStructuralAnchor::new(Vec::new())
            }
            None => {
                self.record_anonymous_anchor_failure(
                    "anonymous type literal has no transported frontend anchor",
                );
                crate::RirStructuralAnchor::new(Vec::new())
            }
        }
    }

    fn record_anonymous_anchor_failure(&mut self, reason: &'static str) {
        if self.payload_error.is_none() {
            self.payload_error = Some(crate::RirPayloadBuildError::InvalidBuilderInput {
                family: "anonymous type anchor",
                reason,
            });
        }
    }

    fn string_literal_anchor(&self, occurrence: u32) -> crate::RirStructuralAnchor {
        let mut segments = self.structural_path.clone();
        segments.push(crate::RirStructuralPathSegment::StringLiteral(occurrence));
        crate::RirStructuralAnchor::new(segments)
    }

    fn read_only_data_anchor(&self, occurrence: u32) -> crate::RirStructuralAnchor {
        let mut segments = self.structural_path.clone();
        segments.push(crate::RirStructuralPathSegment::ReadOnlyData(occurrence));
        crate::RirStructuralAnchor::new(segments)
    }

    fn gen_item(&mut self, item: &Item) {
        match item {
            Item::Function(func) => {
                self.gen_function(func);
            }
            Item::Struct(struct_decl) => {
                self.gen_struct(struct_decl);
            }
            Item::Enum(enum_decl) => {
                self.gen_enum(enum_decl);
            }
            Item::DropFn(drop_fn) => {
                self.gen_drop_fn(drop_fn);
            }
            Item::Extern(extern_block) => {
                self.gen_extern_block(extern_block);
            }
            Item::Const(const_decl) => {
                self.gen_const(const_decl);
            }
            // Error nodes from parser recovery are skipped - errors were already reported
            Item::Error(_) => {}
        }
    }

    /// Convert a TypeExpr to its symbol representation.
    /// For named types, returns the existing symbol. For compound types, interns a new string.
    fn intern_type(&mut self, ty: &TypeExpr) -> Spur {
        match ty {
            TypeExpr::Named(ident) => self.symbol(ident.name),
            TypeExpr::Qualified { segments, .. } => {
                let name = self.render_type_path(segments);
                self.interner.get_or_intern(&name)
            }
            TypeExpr::Unit(_) => self.interner.get_or_intern("()"),
            TypeExpr::Never(_) => self.interner.get_or_intern("!"),
            TypeExpr::Array {
                element, length, ..
            } => {
                // For arrays, we need to construct a string representation
                // Get the element symbol first, then look it up
                let elem_sym = self.intern_type(element);
                let elem_name = self.interner.resolve(&elem_sym);
                // The length component is a literal (`4`), a name referring to
                // a `const` / `comptime` value parameter (`N`), or a
                // comptime-evaluable call (`fact(4)`), all resolved to a
                // concrete value during sema (RUE-16, RUE-309).
                let len_str = self.render_array_length(length);
                let s = format!("[{}; {}]", elem_name, len_str);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::Slice { element, .. } => {
                // Slice type `[T]` (ADR-0043, RUE-322): canonical string is
                // `[elem]` (no length), distinguishing it from `[elem; N]`.
                // Sema recognizes this shape, gates it behind `--preview
                // slices`, and (until the fat-pointer runtime lands) reports it
                // as not-yet-implemented.
                let elem_sym = self.intern_type(element);
                let elem_name = self.interner.resolve(&elem_sym);
                let s = format!("[{}]", elem_name);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::AnonymousStruct { fields, .. } => {
                // For anonymous structs, generate a canonical name representation
                let mut s = String::from("struct { ");
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let field_name = self.symbol(field.name.name);
                    let name = self.interner.resolve(&field_name);
                    let ty_sym = self.intern_type(&field.ty);
                    let ty_name = self.interner.resolve(&ty_sym);
                    s.push_str(name);
                    s.push_str(": ");
                    s.push_str(ty_name);
                }
                s.push_str(" }");
                self.interner.get_or_intern(&s)
            }
            TypeExpr::AnonymousEnum { variants, .. } => {
                // Canonical name representation for an anonymous enum type used
                // in type position (rare — anon enums normally appear as the
                // body of a comptime type function, handled via AnonEnumType).
                let mut s = String::from("enum { ");
                for (i, variant) in variants.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let variant_name = self.symbol(variant.name.name);
                    let name = self.interner.resolve(&variant_name);
                    s.push_str(name);
                    if !variant.payload.is_empty() {
                        s.push('(');
                        for (j, ty) in variant.payload.iter().enumerate() {
                            if j > 0 {
                                s.push_str(", ");
                            }
                            let ty_sym = self.intern_type(ty);
                            s.push_str(self.interner.resolve(&ty_sym));
                        }
                        s.push(')');
                    }
                }
                s.push_str(" }");
                self.interner.get_or_intern(&s)
            }
            TypeExpr::PointerConst { pointee, .. } => {
                // ptr const T
                let pointee_sym = self.intern_type(pointee);
                let pointee_name = self.interner.resolve(&pointee_sym);
                let s = format!("ptr const {}", pointee_name);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::PointerMut { pointee, .. } => {
                // ptr mut T
                let pointee_sym = self.intern_type(pointee);
                let pointee_name = self.interner.resolve(&pointee_sym);
                let s = format!("ptr mut {}", pointee_name);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::TypeCall { name, args, .. } => {
                // Type-function application `Name(arg, ...)` (RUE-241). Encode a
                // canonical `Name(arg1, arg2)` string; sema (`resolve_type`)
                // detects this call syntax and reduces the comptime type call
                // to the monomorphized concrete type. Arguments are interned
                // recursively so nested calls compose
                // (`Result(Option(i32), i32)`).
                let name = self.symbol(name.name);
                let mut s = self.interner.resolve(&name).to_string();
                s.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let arg_sym = self.intern_type(arg);
                    s.push_str(self.interner.resolve(&arg_sym));
                }
                s.push(')');
                self.interner.get_or_intern(&s)
            }
            TypeExpr::QualifiedTypeCall { segments, args, .. } => {
                let mut s = self.render_type_path(segments);
                s.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let arg_sym = self.intern_type(arg);
                    s.push_str(self.interner.resolve(&arg_sym));
                }
                s.push(')');
                self.interner.get_or_intern(&s)
            }
            TypeExpr::IntArg { value, .. } => {
                // Integer type-call argument (RUE-552): canonicalize to its
                // decimal spelling inside the enclosing call's type string,
                // the same form `Str(8)`'s dedicated node produces.
                self.interner.get_or_intern(value.to_string())
            }
            TypeExpr::StrFixed { name, length, .. } => {
                // Fixed-capacity string `Str(N)` with a literal capacity
                // (ADR-0043 Phase 5, RUE-326). Canonicalize to `Name(N)` — the
                // same string the const-capacity `TypeCall` spelling produces —
                // so sema's `resolve_type` reduces both to one `Str(N)` type.
                let name = self.symbol(name.name);
                let callee = self.interner.resolve(&name);
                let s = format!("{}({})", callee, length);
                self.interner.get_or_intern(&s)
            }
        }
    }

    fn render_type_path(&mut self, segments: &[rue_parser::ast::Ident]) -> String {
        let mut s = String::new();
        for (i, segment) in segments.iter().enumerate() {
            if i > 0 {
                s.push('.');
            }
            let segment = self.symbol(segment.name);
            s.push_str(self.interner.resolve(&segment));
        }
        s
    }

    /// Render an array-length component to its canonical string form for the
    /// interned type name (`[element; <this>]`).
    ///
    /// A literal renders as its decimal value, a name as the identifier text,
    /// and a call as `callee(arg, ...)` with each argument rendered by the same
    /// rule so nested calls compose (RUE-309). Sema parses these forms back out
    /// of the type string and folds them to a concrete length (RUE-16).
    fn render_array_length(&mut self, length: &ArrayLength) -> String {
        match length {
            ArrayLength::Literal(n) => n.to_string(),
            ArrayLength::Named(ident) => {
                let name = self.symbol(ident.name);
                self.interner.resolve(&name).to_string()
            }
            ArrayLength::Call { name, args } => {
                let name = self.symbol(name.name);
                let callee = self.interner.resolve(&name).to_owned();
                let rendered: Vec<String> =
                    args.iter().map(|a| self.render_array_length(a)).collect();
                format!("{}({})", callee, rendered.join(", "))
            }
        }
    }

    fn gen_struct(&mut self, struct_decl: &StructDecl) -> InstRef {
        let directives = self.convert_directives(&struct_decl.directives);
        let name = self.symbol(struct_decl.name.name);
        let fields: Vec<_> = struct_decl
            .fields
            .iter()
            .enumerate()
            .map(|(index, f)| {
                let field_name = self.symbol(f.name.name);
                let field_type = self.intern_type_at(
                    crate::RirStructuralPathSegment::FieldType(index as u32),
                    &f.ty,
                );
                (field_name, field_type)
            })
            .collect();
        // Generate each method defined inline in the struct
        let methods: Vec<_> = struct_decl
            .methods
            .iter()
            .enumerate()
            .map(|(index, m)| {
                self.with_structural_segment(
                    crate::RirStructuralPathSegment::Method(index as u32),
                    |this| this.gen_method(m),
                )
            })
            .collect();
        self.rir
            .add_struct_decl(
                &directives,
                struct_decl.visibility == Visibility::Public,
                struct_decl.is_linear,
                name,
                &fields,
                &methods,
                struct_decl.span,
            )
            .record_failure(&mut self.payload_error)
    }

    fn gen_enum(&mut self, enum_decl: &EnumDecl) -> InstRef {
        let name = self.symbol(enum_decl.name.name);
        let variants: Vec<_> = enum_decl
            .variants
            .iter()
            .map(|v| self.symbol(v.name.name))
            .collect();
        // Encode tuple-variant payloads (RUE-221) as a self-describing flat
        // sequence: for each variant, a count `k` followed by `k` payload
        // type-name symbols. Discriminant-only variants contribute a `0`.
        // The whole region is omitted (len 0) when no variant carries data.
        let payload_types: Vec<Vec<Spur>> = enum_decl
            .variants
            .iter()
            .enumerate()
            .map(|(variant_index, variant)| {
                variant
                    .payload
                    .iter()
                    .enumerate()
                    .map(|(payload_index, ty)| {
                        self.intern_type_at(
                            crate::RirStructuralPathSegment::VariantPayload {
                                variant: variant_index as u32,
                                payload: payload_index as u32,
                            },
                            ty,
                        )
                    })
                    .collect()
            })
            .collect();
        self.rir
            .add_enum_decl(
                enum_decl.visibility == Visibility::Public,
                name,
                &variants,
                &payload_types,
                enum_decl.span,
            )
            .record_failure(&mut self.payload_error)
    }

    fn gen_const(&mut self, const_decl: &ConstDecl) -> InstRef {
        self.with_producer_root(&const_decl.init, |this| this.gen_const_body(const_decl))
    }

    fn gen_const_body(&mut self, const_decl: &ConstDecl) -> InstRef {
        let directives = self.convert_directives(&const_decl.directives);
        let name = self.symbol(const_decl.name.name);
        let ty = const_decl
            .ty
            .as_ref()
            .map(|t| self.intern_type_at(crate::RirStructuralPathSegment::ReturnType, t));
        let init = self.gen_expr_at(crate::RirStructuralPathSegment::Body, &const_decl.init);

        self.rir
            .add_const_decl(
                &directives,
                const_decl.visibility == Visibility::Public,
                name,
                ty,
                init,
                const_decl.span,
            )
            .record_failure(&mut self.payload_error)
    }

    fn gen_drop_fn(&mut self, drop_fn: &DropFn) -> InstRef {
        self.with_producer_root(&drop_fn.body, |this| this.gen_drop_fn_body(drop_fn))
    }

    fn gen_drop_fn_body(&mut self, drop_fn: &DropFn) -> InstRef {
        let type_name = self.symbol(drop_fn.type_name.name);

        // Generate the body expression
        let body = self.gen_expr_at(crate::RirStructuralPathSegment::Body, &drop_fn.body);

        self.rir.add_inst(Inst {
            data: InstData::DropFnDecl { type_name, body },
            span: drop_fn.span,
        })
    }

    fn gen_method(&mut self, method: &Method) -> InstRef {
        self.with_producer_root(&method.body, |this| this.gen_method_body(method))
    }

    fn gen_method_body(&mut self, method: &Method) -> InstRef {
        // Convert directives
        let directives = self.convert_directives(&method.directives);

        // Get the method name (already a Symbol) and return type
        let name = self.symbol(method.name.name);
        let return_type = match &method.return_type {
            Some(ty) => self.intern_type_at(crate::RirStructuralPathSegment::ReturnType, ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters (excluding self, which is handled specially by sema)
        let params: Vec<_> = method
            .params
            .iter()
            .enumerate()
            .map(|(index, p)| RirParam {
                name: self.symbol(p.name.name),
                ty: self.intern_type_at(
                    crate::RirStructuralPathSegment::ParameterType(index as u32),
                    &p.ty,
                ),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.mode == ParamMode::Comptime,
                span: p.name.span,
            })
            .collect();
        // Generate body expression
        let body = self.gen_expr_at(crate::RirStructuralPathSegment::Body, &method.body);

        // Track whether this method has a self receiver (method vs associated
        // function) and, if so, the receiver's passing mode (`borrow self` /
        // `inout self` / bare by-value `self`, RUE-15).
        let has_self = method.receiver.is_some();
        let self_mode = match &method.receiver {
            Some(receiver) => self.convert_param_mode(receiver.mode),
            None => RirParamMode::Normal,
        };
        // `mut self` (by-value receiver binding mutably in the body) is
        // carried separately from the mode so signatures stay mode-only.
        let self_is_mut = method
            .receiver
            .as_ref()
            .is_some_and(|receiver| receiver.is_mut);

        // Emit methods as FnDecl instructions with has_self flag.
        // Sema uses has_self to add the implicit self parameter for methods,
        // and self_mode to add it in the declared borrow/inout/by-value mode.
        // Methods don't have their own visibility - they're accessible if the type is accessible.
        // Methods cannot be marked unchecked (that's a function-level modifier).
        let decl = self
            .rir
            .add_fn_decl(
                &directives,
                false,
                false,
                false,
                // Methods are never C exports.
                false,
                name,
                &params,
                return_type,
                body,
                has_self,
                self_mode,
                self_is_mut,
                method.span,
            )
            .record_failure(&mut self.payload_error);

        decl
    }

    /// Convert AST directives to RIR directives
    fn convert_directives(&mut self, directives: &[Directive]) -> Vec<RirDirective> {
        directives
            .iter()
            .map(|d| RirDirective {
                name: self.symbol(d.name.name),
                args: d
                    .args
                    .iter()
                    .map(|arg| match arg {
                        DirectiveArg::Ident(ident) => self.symbol(ident.name),
                    })
                    .collect(),
                span: d.span,
            })
            .collect()
    }

    /// Convert AST ParamMode to RIR RirParamMode.
    ///
    /// Comptime is orthogonal to the RIR passing mode and is represented by
    /// `RirParam::is_comptime`, so comptime parameters use the normal passing
    /// mode here.
    fn convert_param_mode(&self, mode: ParamMode) -> RirParamMode {
        match mode {
            ParamMode::Normal => RirParamMode::Normal,
            ParamMode::Comptime => RirParamMode::Normal,
            ParamMode::Inout => RirParamMode::Inout,
            ParamMode::Borrow => RirParamMode::Borrow,
        }
    }

    /// Convert AST ArgMode to RIR RirArgMode
    fn convert_arg_mode(&self, mode: ArgMode) -> RirArgMode {
        match mode {
            ArgMode::Normal => RirArgMode::Normal,
            ArgMode::Inout => RirArgMode::Inout,
            ArgMode::Borrow => RirArgMode::Borrow,
        }
    }

    /// Convert a CallArg to RirCallArg
    fn convert_call_arg(&mut self, arg: &CallArg) -> RirCallArg {
        RirCallArg {
            value: self.gen_expr(&arg.expr),
            mode: self.convert_arg_mode(arg.mode),
        }
    }

    fn gen_function(&mut self, func: &Function) -> InstRef {
        self.with_producer_root(&func.body, |this| this.gen_function_body(func))
    }

    fn gen_function_body(&mut self, func: &Function) -> InstRef {
        // Convert directives
        let directives = self.convert_directives(&func.directives);

        // Get the function name (already a Symbol) and return type
        let name = self.symbol(func.name.name);
        let return_type = match &func.return_type {
            Some(ty) => self.intern_type_at(crate::RirStructuralPathSegment::ReturnType, ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters
        let params: Vec<_> = func
            .params
            .iter()
            .enumerate()
            .map(|(index, p)| RirParam {
                name: self.symbol(p.name.name),
                ty: self.intern_type_at(
                    crate::RirStructuralPathSegment::ParameterType(index as u32),
                    &p.ty,
                ),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.mode == ParamMode::Comptime,
                span: p.name.span,
            })
            .collect();
        // Generate body expression
        let body = self.gen_expr_at(crate::RirStructuralPathSegment::Body, &func.body);

        // Create function declaration instruction
        // Regular functions don't have a self receiver
        let decl = self
            .rir
            .add_fn_decl(
                &directives,
                func.visibility == Visibility::Public,
                func.is_unchecked,
                false,
                // `pub extern "C" fn` marks a Rue-to-C export (ADR-0064 P4).
                func.export_abi.is_some(),
                name,
                &params,
                return_type,
                body,
                false,
                RirParamMode::Normal,
                false,
                func.span,
            )
            .record_failure(&mut self.payload_error);

        decl
    }

    /// Lower an `extern "C" { ... }` block: each member becomes a body-less
    /// foreign `FnDecl` (`is_extern = true`) with a synthesized unit placeholder
    /// body. Sema never analyzes and codegen never emits the placeholder; a call
    /// to the declaration lowers to an undefined linker symbol (ADR-0064).
    fn gen_extern_block(&mut self, extern_block: &ExternBlock) {
        for foreign in &extern_block.fns {
            self.with_bodyless_producer_root(|this| this.gen_extern_fn(foreign));
        }
    }

    fn gen_extern_fn(&mut self, foreign: &ExternFn) -> InstRef {
        let name = self.symbol(foreign.name.name);
        let return_type = match &foreign.return_type {
            Some(ty) => self.intern_type_at(crate::RirStructuralPathSegment::ReturnType, ty),
            None => self.interner.get_or_intern("()"),
        };
        let params: Vec<_> = foreign
            .params
            .iter()
            .enumerate()
            .map(|(index, p)| RirParam {
                name: self.symbol(p.name.name),
                ty: self.intern_type_at(
                    crate::RirStructuralPathSegment::ParameterType(index as u32),
                    &p.ty,
                ),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.mode == ParamMode::Comptime,
                span: p.name.span,
            })
            .collect();
        // A foreign declaration has no body; synthesize a unit placeholder that
        // is never analyzed or code-generated (guarded by `is_extern`).
        let body = self.rir.add_inst(Inst {
            data: InstData::UnitConst,
            span: foreign.span,
        });
        self.rir
            .add_fn_decl(
                &[],
                // Foreign declarations carry no Rue visibility modifier (treated
                // as a private free function for resolution) and are not
                // `unchecked` functions — the checked-context gate is enforced
                // at the call site instead (ADR-0064 unchecked-only ruling).
                false,
                false,
                true,
                // A foreign import is not an export.
                false,
                name,
                &params,
                return_type,
                body,
                false,
                RirParamMode::Normal,
                false,
                foreign.span,
            )
            .record_failure(&mut self.payload_error)
    }

    fn gen_expr(&mut self, expr: &Expr) -> InstRef {
        match expr {
            Expr::Int(lit) => self.rir.add_inst(Inst {
                data: InstData::IntConst(lit.value),
                span: lit.span,
            }),
            Expr::Bool(lit) => self.rir.add_inst(Inst {
                data: InstData::BoolConst(lit.value),
                span: lit.span,
            }),
            Expr::String(lit) => self.rir.add_inst(Inst {
                data: InstData::StringConst {
                    content: self.symbol(lit.value),
                    anchor: self.string_literal_anchor(0),
                },
                span: lit.span,
            }),
            Expr::Unit(lit) => self.rir.add_inst(Inst {
                data: InstData::UnitConst,
                span: lit.span,
            }),
            Expr::Ident(ident) => self.rir.add_inst(Inst {
                data: InstData::VarRef {
                    name: self.symbol(ident.name),
                    anchor: Some(self.read_only_data_anchor(0)),
                },
                span: ident.span,
            }),
            Expr::Binary(bin) => {
                let lhs = self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &bin.left);
                let rhs = self.gen_expr_at(crate::RirStructuralPathSegment::Operand(1), &bin.right);
                let data = match bin.op {
                    BinaryOp::Add => InstData::Add { lhs, rhs },
                    BinaryOp::Sub => InstData::Sub { lhs, rhs },
                    BinaryOp::Mul => InstData::Mul { lhs, rhs },
                    BinaryOp::Div => InstData::Div { lhs, rhs },
                    BinaryOp::Mod => InstData::Mod { lhs, rhs },
                    BinaryOp::Eq => InstData::Eq { lhs, rhs },
                    BinaryOp::Ne => InstData::Ne { lhs, rhs },
                    BinaryOp::Lt => InstData::Lt { lhs, rhs },
                    BinaryOp::Gt => InstData::Gt { lhs, rhs },
                    BinaryOp::Le => InstData::Le { lhs, rhs },
                    BinaryOp::Ge => InstData::Ge { lhs, rhs },
                    BinaryOp::And => InstData::And { lhs, rhs },
                    BinaryOp::Or => InstData::Or { lhs, rhs },
                    BinaryOp::BitAnd => InstData::BitAnd { lhs, rhs },
                    BinaryOp::BitOr => InstData::BitOr { lhs, rhs },
                    BinaryOp::BitXor => InstData::BitXor { lhs, rhs },
                    BinaryOp::Shl => InstData::Shl { lhs, rhs },
                    BinaryOp::Shr => InstData::Shr { lhs, rhs },
                };
                self.rir.add_inst(Inst {
                    data,
                    span: bin.span,
                })
            }
            Expr::Unary(un) => {
                let operand =
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &un.operand);
                let data = match un.op {
                    UnaryOp::Neg => InstData::Neg { operand },
                    UnaryOp::Not => InstData::Not { operand },
                    UnaryOp::BitNot => InstData::BitNot { operand },
                };
                self.rir.add_inst(Inst {
                    data,
                    span: un.span,
                })
            }
            Expr::Try(try_expr) => {
                let operand = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &try_expr.operand,
                );
                self.rir.add_inst(Inst {
                    data: InstData::Try { operand },
                    span: try_expr.span,
                })
            }
            Expr::Paren(paren) => {
                // Parentheses are transparent in the IR - just generate the inner expression
                self.gen_expr(&paren.inner)
            }
            Expr::Block(block) => self.gen_block(block),
            Expr::If(if_expr) => {
                let cond =
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &if_expr.cond);
                let then_block = self
                    .with_structural_segment(crate::RirStructuralPathSegment::Branch(0), |this| {
                        this.gen_block(&if_expr.then_block)
                    });
                let else_block = if_expr.else_block.as_ref().map(|block| {
                    self.with_structural_segment(
                        crate::RirStructuralPathSegment::Branch(1),
                        |this| this.gen_block(block),
                    )
                });

                self.rir.add_inst(Inst {
                    data: InstData::Branch {
                        cond,
                        then_block,
                        else_block,
                    },
                    span: if_expr.span,
                })
            }
            Expr::While(while_expr) => {
                let cond = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &while_expr.cond,
                );
                let body = self
                    .with_structural_segment(crate::RirStructuralPathSegment::Branch(0), |this| {
                        this.gen_block(&while_expr.body)
                    });
                self.rir.add_inst(Inst {
                    data: InstData::Loop { cond, body },
                    span: while_expr.span,
                })
            }
            Expr::For(for_expr) => self.gen_for(for_expr),
            Expr::Loop(loop_expr) => {
                let body = self
                    .with_structural_segment(crate::RirStructuralPathSegment::Branch(0), |this| {
                        this.gen_block(&loop_expr.body)
                    });
                self.rir.add_inst(Inst {
                    data: InstData::InfiniteLoop {
                        body,
                        iter_borrow: None,
                    },
                    span: loop_expr.span,
                })
            }
            Expr::Match(match_expr) => {
                let scrutinee = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &match_expr.scrutinee,
                );
                let arms: Vec<_> = match_expr
                    .arms
                    .iter()
                    .enumerate()
                    .map(|(index, arm)| {
                        self.with_structural_segment(
                            crate::RirStructuralPathSegment::MatchArm(index as u32),
                            |this| {
                                let pattern = this.with_structural_segment(
                                    crate::RirStructuralPathSegment::Operand(0),
                                    |this| this.gen_pattern(&arm.pattern),
                                );
                                let body = this.gen_expr_at(
                                    crate::RirStructuralPathSegment::Operand(1),
                                    &arm.body,
                                );
                                (pattern, body)
                            },
                        )
                    })
                    .collect();
                self.rir
                    .add_match(scrutinee, &arms, match_expr.span)
                    .record_failure(&mut self.payload_error)
            }
            Expr::Call(call) => {
                let args: Vec<_> = call
                    .args
                    .iter()
                    .enumerate()
                    .map(|(index, arg)| {
                        self.with_structural_segment(
                            crate::RirStructuralPathSegment::Operand(index as u32),
                            |this| this.convert_call_arg(arg),
                        )
                    })
                    .collect();
                let name = self.symbol(call.name.name);
                self.rir
                    .add_call(name, &args, call.span)
                    .record_failure(&mut self.payload_error)
            }
            Expr::Break(break_expr) => {
                let value = break_expr.value.as_ref().map(|value| {
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), value)
                });
                self.rir.add_inst(Inst {
                    data: InstData::Break { value },
                    span: break_expr.span,
                })
            }
            Expr::Continue(continue_expr) => self.rir.add_inst(Inst {
                data: InstData::Continue,
                span: continue_expr.span,
            }),
            Expr::Return(return_expr) => {
                let value = return_expr.value.as_ref().map(|value| {
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), value)
                });
                self.rir.add_inst(Inst {
                    data: InstData::Ret(value),
                    span: return_expr.span,
                })
            }
            Expr::StructLit(struct_lit) => {
                // Generate module reference if this is a qualified struct literal
                let module = struct_lit.base.as_ref().map(|base_expr| {
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), base_expr)
                });

                // Inline type-constructor struct-literal head `F(args) { ... }`
                // (RUE-596): generate the constructor call `F(args)` as its own
                // instruction; sema reduces it to the struct type at comptime. When
                // the head is module-qualified (`std.tuple.Pair(i64, i32) { ... }`,
                // RUE-951) the constructor is reached through the module base, so
                // emit a method call on it exactly as the pattern ctor head does
                // (RUE-947); a bare call would fail to resolve `Pair` locally.
                let ctor_head = struct_lit.ctor_args.as_ref().map(|args| {
                    let arg_refs: Vec<_> = args
                        .iter()
                        .enumerate()
                        .map(|(index, arg)| {
                            self.with_structural_segment(
                                crate::RirStructuralPathSegment::Operand(index as u32 + 1),
                                |this| this.convert_call_arg(arg),
                            )
                        })
                        .collect();
                    let name = self.symbol(struct_lit.name.name);
                    match module {
                        Some(base) => {
                            self.rir
                                .add_method_call(base, name, &arg_refs, struct_lit.span)
                        }
                        None => self.rir.add_call(name, &arg_refs, struct_lit.span),
                    }
                    .record_failure(&mut self.payload_error)
                });

                let fields: Vec<_> = struct_lit
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(index, f)| {
                        let field_value = self.gen_expr_at(
                            crate::RirStructuralPathSegment::FieldType(index as u32),
                            &f.value,
                        );
                        (self.symbol(f.name.name), field_value)
                    })
                    .collect();
                // Field-init shorthand (`P { x }`, RUE-613) is fully desugared to
                // `x: x` above; carry the first shorthand field's span so Sema can
                // gate the form behind its preview flag.
                let shorthand_span = struct_lit
                    .fields
                    .iter()
                    .find(|f| f.shorthand)
                    .map(|f| f.span);

                let type_name = self.symbol(struct_lit.name.name);
                self.rir
                    .add_struct_init(
                        module,
                        ctor_head,
                        type_name,
                        &fields,
                        shorthand_span,
                        struct_lit.span,
                    )
                    .record_failure(&mut self.payload_error)
            }
            Expr::Field(field_expr) => {
                let base = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &field_expr.base,
                );

                self.rir.add_inst(Inst {
                    data: InstData::FieldGet {
                        base,
                        field: self.symbol(field_expr.field.name),
                    },
                    span: field_expr.span,
                })
            }
            Expr::IntrinsicCall(intrinsic) => {
                let name = self.symbol(intrinsic.name.name);
                let intrinsic_name_str = self.interner.resolve(&name);

                // `@offset_of(T, field)` (RUE-301) is compiler-mediated field
                // addressing: the first argument is a type and the second
                // names one of its fields. The parser parses the type position
                // with the canonical type grammar (RUE-788), so the first
                // argument always arrives as `IntrinsicArg::Type`. Lower the
                // pair into a dedicated `OffsetOf` node so Sema can compute
                // the offset from the layout it assigns, rather than the user
                // hardcoding a literal.
                if intrinsic_name_str == OFFSET_OF_INTRINSIC && intrinsic.args.len() == 2 {
                    if let (IntrinsicArg::Type(ty), IntrinsicArg::Expr(Expr::Ident(field))) =
                        (&intrinsic.args[0], &intrinsic.args[1])
                    {
                        let type_arg = self.intern_type(ty);
                        return self.rir.add_inst(Inst {
                            data: InstData::OffsetOf {
                                type_arg,
                                field: self.symbol(field.name),
                            },
                            span: intrinsic.span,
                        });
                    }
                    // Fall through to the generic expression-intrinsic path,
                    // which surfaces a proper diagnostic (wrong argument shape)
                    // during semantic analysis.
                }

                // Type intrinsics at the documented arity lower to a dedicated
                // node; any other arity falls through so semantic analysis
                // reports the arity error with every argument accounted for.
                let is_type_intrinsic = TYPE_INTRINSICS.contains(&intrinsic_name_str);
                if is_type_intrinsic
                    && intrinsic.args.len() == 1
                    && let IntrinsicArg::Type(ty) = &intrinsic.args[0]
                {
                    let type_arg = self.intern_type(ty);
                    return self.rir.add_inst(Inst {
                        data: InstData::TypeIntrinsic { name, type_arg },
                        span: intrinsic.span,
                    });
                }

                // Otherwise, treat as an expression intrinsic
                let args: Vec<_> = intrinsic
                    .args
                    .iter()
                    .enumerate()
                    .map(|(index, a)| match a {
                        IntrinsicArg::Expr(expr) => self.gen_expr_at(
                            crate::RirStructuralPathSegment::Operand(index as u32),
                            expr,
                        ),
                        // A type argument to an expression intrinsic (e.g. the
                        // `()` in `@syscall(a, (), b)`) is invalid, but it must
                        // NOT be dropped: that would shift the later arguments
                        // into earlier slots and silently miscompile. Lower it
                        // to a TypeConst placeholder so the argument count is
                        // preserved and Sema reports a proper type error.
                        IntrinsicArg::Type(ty) => {
                            let type_name = self.intern_type(ty);
                            self.rir.add_inst(Inst {
                                data: InstData::TypeConst { type_name },
                                span: ty.span(),
                            })
                        }
                    })
                    .collect();
                self.rir
                    .add_intrinsic(name, &args, intrinsic.span)
                    .record_failure(&mut self.payload_error)
            }
            Expr::ArrayLit(array_lit) => {
                if let Some(count) = &array_lit.repeat {
                    // Repeat form `[value; count]` (RUE-235): the single value
                    // is evaluated once; the count is carried symbolically and
                    // resolved during sema via the array-length const-eval path.
                    let value = self.gen_expr_at(
                        crate::RirStructuralPathSegment::Operand(0),
                        &array_lit.elements[0],
                    );
                    let count = match count {
                        ArrayLength::Literal(n) => RepeatCount::Literal(*n),
                        ArrayLength::Named(ident) => RepeatCount::Named(self.symbol(ident.name)),
                        // The array-literal repeat grammar (`[value; count]`)
                        // only parses a literal or a bare name, never a call,
                        // so this arm is unreachable. The call form is accepted
                        // only in array-*type* length position (RUE-309).
                        ArrayLength::Call { .. } => {
                            unreachable!("array repeat count never parses a call form")
                        }
                    };
                    self.rir.add_inst(Inst {
                        data: InstData::ArrayRepeat { value, count },
                        span: array_lit.span,
                    })
                } else {
                    let elements: Vec<_> = array_lit
                        .elements
                        .iter()
                        .enumerate()
                        .map(|(index, element)| {
                            self.gen_expr_at(
                                crate::RirStructuralPathSegment::Operand(index as u32),
                                element,
                            )
                        })
                        .collect();
                    self.rir
                        .add_array_init(&elements, array_lit.span)
                        .record_failure(&mut self.payload_error)
                }
            }
            Expr::Index(index_expr) => {
                let base = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &index_expr.base,
                );
                let index = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(1),
                    &index_expr.index,
                );

                self.rir.add_inst(Inst {
                    data: InstData::IndexGet { base, index },
                    span: index_expr.span,
                })
            }
            Expr::Path(path_expr) => {
                // Generate module reference if this is a qualified path
                let module = path_expr.base.as_ref().map(|base_expr| {
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), base_expr)
                });

                self.rir.add_inst(Inst {
                    data: InstData::EnumVariant {
                        module,
                        type_name: self.symbol(path_expr.type_name.name),
                        variant: self.symbol(path_expr.variant.name),
                    },
                    span: path_expr.span,
                })
            }
            Expr::MethodCall(method_call) => {
                let receiver = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &method_call.receiver,
                );
                let args: Vec<_> = method_call
                    .args
                    .iter()
                    .enumerate()
                    .map(|(index, arg)| {
                        self.with_structural_segment(
                            crate::RirStructuralPathSegment::Operand(index as u32 + 1),
                            |this| this.convert_call_arg(arg),
                        )
                    })
                    .collect();
                let method = self.symbol(method_call.method.name);
                self.rir
                    .add_method_call(receiver, method, &args, method_call.span)
                    .record_failure(&mut self.payload_error)
            }
            Expr::SelfExpr(self_expr) => {
                // `self` in method bodies is just a variable reference to the implicit self parameter
                let name = self.interner.get_or_intern("self");
                self.rir.add_inst(Inst {
                    data: InstData::VarRef { name, anchor: None },
                    span: self_expr.span,
                })
            }
            Expr::Comptime(comptime_block) => {
                // Generate the inner expression, wrapped in a Comptime instruction
                // The semantic analyzer will evaluate this at compile time
                let inner_expr = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &comptime_block.expr,
                );
                self.rir.add_inst(Inst {
                    data: InstData::Comptime { expr: inner_expr },
                    span: comptime_block.span,
                })
            }
            Expr::Checked(checked_block) => {
                // Generate the inner expression, wrapped in a Checked instruction
                // Unchecked operations are only allowed inside checked blocks
                let inner_expr = self.gen_expr_at(
                    crate::RirStructuralPathSegment::Operand(0),
                    &checked_block.expr,
                );
                self.rir.add_inst(Inst {
                    data: InstData::Checked { expr: inner_expr },
                    span: checked_block.span,
                })
            }
            Expr::TypeLit(type_lit) => {
                // Generate a type constant instruction for type-as-value expressions
                match &type_lit.type_expr {
                    TypeExpr::AnonymousStruct {
                        fields, methods, ..
                    } => {
                        let anchor = self.anonymous_type_anchor(
                            type_lit.type_expr.span(),
                            crate::AnonymousTypeSiteKind::Struct,
                        );
                        // Generate an anonymous struct type instruction with methods
                        let field_decls: Vec<(Spur, Spur)> = fields
                            .iter()
                            .enumerate()
                            .map(|(index, f)| {
                                let name = self.symbol(f.name.name);
                                let ty = self.intern_type_at(
                                    crate::RirStructuralPathSegment::FieldType(index as u32),
                                    &f.ty,
                                );
                                (name, ty)
                            })
                            .collect();
                        // Generate each method inside the anonymous struct
                        // (reusing gen_method, which generates FnDecl instructions)
                        let method_refs: Vec<InstRef> = methods
                            .iter()
                            .enumerate()
                            .map(|(index, method)| {
                                self.with_structural_segment(
                                    crate::RirStructuralPathSegment::Method(index as u32),
                                    |this| this.gen_method(method),
                                )
                            })
                            .collect();
                        self.rir
                            .add_anon_struct_type(&field_decls, &method_refs, anchor, type_lit.span)
                            .record_failure(&mut self.payload_error)
                    }
                    TypeExpr::AnonymousEnum { variants, .. } => {
                        // Generate an anonymous enum type instruction. Variant
                        // names and tuple-variant payloads are encoded exactly
                        // as `gen_enum` does for a top-level `enum` declaration
                        // (RUE-221, ADR-0038).
                        let variant_syms: Vec<Spur> =
                            variants.iter().map(|v| self.symbol(v.name.name)).collect();
                        let payload_types: Vec<Vec<Spur>> = variants
                            .iter()
                            .enumerate()
                            .map(|(variant_index, variant)| {
                                variant
                                    .payload
                                    .iter()
                                    .enumerate()
                                    .map(|(payload_index, ty)| {
                                        self.intern_type_at(
                                            crate::RirStructuralPathSegment::VariantPayload {
                                                variant: variant_index as u32,
                                                payload: payload_index as u32,
                                            },
                                            ty,
                                        )
                                    })
                                    .collect()
                            })
                            .collect();
                        let anchor = self.anonymous_type_anchor(
                            type_lit.type_expr.span(),
                            crate::AnonymousTypeSiteKind::Enum,
                        );
                        self.rir
                            .add_anon_enum_type(
                                &variant_syms,
                                &payload_types,
                                anchor,
                                type_lit.span,
                            )
                            .record_failure(&mut self.payload_error)
                    }
                    _ => {
                        // For named types, unit, never, arrays, and pointers, generate TypeConst
                        let type_name = match &type_lit.type_expr {
                            TypeExpr::Named(ident) => self.symbol(ident.name),
                            TypeExpr::Qualified { .. } => self.intern_type(&type_lit.type_expr),
                            TypeExpr::Unit(_) => self.interner.get_or_intern_static("()"),
                            TypeExpr::Never(_) => self.interner.get_or_intern_static("!"),
                            TypeExpr::Array { .. } => {
                                // Array types as values are not yet supported
                                // For now, use a placeholder
                                self.interner.get_or_intern_static("array")
                            }
                            TypeExpr::Slice { .. } => {
                                // Slice type `[T]` in value position (ADR-0043,
                                // RUE-322). Intern its canonical string; sema
                                // gates it behind `--preview slices` and reports
                                // it not-yet-implemented.
                                self.intern_type(&type_lit.type_expr)
                            }
                            TypeExpr::AnonymousStruct { .. } | TypeExpr::AnonymousEnum { .. } => {
                                unreachable!("handled above")
                            }
                            TypeExpr::PointerConst { .. } | TypeExpr::PointerMut { .. } => {
                                // Pointer types as values - use intern_type to get representation
                                self.intern_type(&type_lit.type_expr)
                            }
                            TypeExpr::TypeCall { .. } | TypeExpr::QualifiedTypeCall { .. } => {
                                // A type-function application in *value* position
                                // (`let R = Result(i32, i32)`) is parsed as an
                                // ordinary call expression, not a TypeLit, so it
                                // does not normally reach here. Intern its
                                // canonical string for completeness (RUE-241).
                                self.intern_type(&type_lit.type_expr)
                            }
                            TypeExpr::StrFixed { .. } => {
                                // Fixed-capacity string `Str(N)` in value position
                                // (ADR-0043 Phase 5, RUE-326). Intern its canonical
                                // `Str(N)` string for completeness; sema resolves it.
                                self.intern_type(&type_lit.type_expr)
                            }
                            TypeExpr::IntArg { .. } => {
                                // Only produced inside type-call argument lists
                                // (RUE-552); a bare integer is never a TypeLit.
                                unreachable!("IntArg outside a type-call argument list")
                            }
                        };
                        self.rir.add_inst(Inst {
                            data: InstData::TypeConst { type_name },
                            span: type_lit.span,
                        })
                    }
                }
            }
            // Error nodes from parser recovery - generate a unit constant as a placeholder
            // The error was already reported during parsing
            Expr::Error(span) => self.rir.add_inst(Inst {
                data: InstData::UnitConst,
                span: *span,
            }),
        }
    }

    fn gen_pattern(&mut self, pattern: &Pattern) -> RirPattern {
        match pattern {
            Pattern::Wildcard(span) => RirPattern::Wildcard(*span),
            // Keep the raw u64 magnitude and sign: Sema range-checks the
            // literal against the scrutinee type (E0800/E0801) before
            // converting it to a comparison value, so out-of-range patterns
            // are rejected instead of silently wrapping (RUE-74).
            Pattern::Int(lit) => RirPattern::Int {
                value: lit.value,
                negative: false,
                span: lit.span,
            },
            Pattern::NegInt(lit) => RirPattern::Int {
                value: lit.value,
                negative: true,
                span: lit.span,
            },
            Pattern::Bool(lit) => RirPattern::Bool(lit.value, lit.span),
            Pattern::Path(path) => {
                // If there's a base expression (module reference), generate it first
                let module = path.base.as_ref().map(|base| {
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), base)
                });
                // Inline type-constructor pattern head `F(args).Variant(..)`
                // (RUE-596): generate the constructor call `F(args)` as its own
                // instruction; sema reduces it to the enum type at comptime. When
                // the head is module-qualified (`std.result.Result(i32, i32).Ok`,
                // RUE-947) the constructor is reached through the module base, so
                // emit a method call on it exactly as the construction head does;
                // a bare call would fail to resolve `Result` in local scope.
                let ctor_head = path.ctor_args.as_ref().map(|args| {
                    let arg_refs: Vec<_> = args
                        .iter()
                        .enumerate()
                        .map(|(index, arg)| {
                            self.with_structural_segment(
                                crate::RirStructuralPathSegment::Operand(index as u32 + 1),
                                |this| this.convert_call_arg(arg),
                            )
                        })
                        .collect();
                    let name = self.symbol(path.type_name.name);
                    match module {
                        Some(base) => self.rir.add_method_call(base, name, &arg_refs, path.span),
                        None => self.rir.add_call(name, &arg_refs, path.span),
                    }
                    .record_failure(&mut self.payload_error)
                });
                // Payload binding names for a tuple-variant pattern (RUE-221).
                let bindings: Vec<Spur> =
                    path.bindings.iter().map(|b| self.symbol(b.name)).collect();
                RirPattern::Path {
                    module,
                    ctor_head,
                    type_name: self.symbol(path.type_name.name),
                    variant: self.symbol(path.variant.name),
                    bindings,
                    span: path.span,
                }
            }
        }
    }

    fn gen_block(&mut self, block: &rue_parser::BlockExpr) -> InstRef {
        self.with_structural_segment(crate::RirStructuralPathSegment::Body, |this| {
            this.gen_block_contents(block)
        })
    }

    fn gen_block_contents(&mut self, block: &rue_parser::BlockExpr) -> InstRef {
        if block.statements.is_empty() {
            // No statements, just the final expression
            self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &block.expr)
        } else {
            // Collect all instruction refs for the block
            // statements + 1 for the final expression
            let mut inst_refs = Vec::with_capacity(block.statements.len() + 1);

            // Generate all statements first
            for (index, stmt) in block.statements.iter().enumerate() {
                let inst_ref = self.with_structural_segment(
                    crate::RirStructuralPathSegment::Statement(index as u32),
                    |this| this.gen_statement(stmt),
                );
                inst_refs.push(inst_ref.as_u32());
            }

            // Generate the final expression
            let final_expr =
                self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &block.expr);
            inst_refs.push(final_expr.as_u32());

            // Store the refs in extra data
            let refs: Vec<_> = inst_refs.into_iter().map(InstRef::from_raw).collect();
            self.rir
                .add_block(&refs, block.span)
                .record_failure(&mut self.payload_error)
        }
    }

    /// Desugar a `for <binder> in <iterable> { body }` loop (RUE-220).
    ///
    /// Layer 1 of the iteration model: a built-in `for` over the
    /// compiler-known iterables, in read/borrow mode, with no iterator object
    /// and no lifetimes. The loop holds a scoped read of the collection; a
    /// `usize` position value threads through a `loop`, and the element is
    /// projected each iteration (ADR-0037 / RUE-219 layer-1 sketch):
    ///
    /// ```text
    /// { let c = coll; let mut p = 0; let len = <bound>;
    ///   loop {
    ///     if p >= len { break }
    ///     let x = <get(c, p)>;
    ///     p = <advance(c, p)>;   // advanced BEFORE the body so `continue` still steps
    ///     body
    ///   } }
    /// ```
    ///
    /// The type-dependent pieces are compiler-internal RIR operations that
    /// Sema resolves by the collection's type (dispatching the three iterable
    /// kinds — array, String byte view, String `.chars()` / `.chars_lossy()`
    /// scalar views): [`InternalIntrinsic::IterLen`], and for the char views
    /// the scalar/next strict operations (trap on invalid UTF-8) or their lossy
    /// counterparts (substitute U+FFFD). Everything
    /// else reuses the ordinary
    /// loop/branch/break/index lowering, so move-checking, drop elaboration,
    /// and codegen come for free. The whole thing is preview-gated in Sema at
    /// [`InternalIntrinsic::IterLen`], which every for-loop emits.
    fn gen_for(&mut self, for_expr: &rue_parser::ForExpr) -> InstRef {
        let span = for_expr.span;
        let n = self.for_counter;
        self.for_counter += 1;

        // Recognize the `.chars()` / `.chars_lossy()` scalar views
        // syntactically: `for c in s.chars()` iterates Unicode scalars and traps
        // on invalid UTF-8, `for c in s.chars_lossy()` iterates scalars but
        // substitutes U+FFFD for invalid sequences (ADR-0035). Everything else
        // iterates by position (array element / String byte). The receiver of
        // the call is the actual collection.
        let (coll_expr, is_chars, is_lossy): (&Expr, bool, bool) = match &*for_expr.iterable {
            Expr::MethodCall(mc) if mc.args.is_empty() => {
                let method = self.symbol(mc.method.name);
                match self.interner.resolve(&method) {
                    "chars" => (&mc.receiver, true, false),
                    "chars_lossy" => (&mc.receiver, true, true),
                    _ => (&*for_expr.iterable, false, false),
                }
            }
            other => (other, false, false),
        };

        let mut outer_stmts: Vec<u32> = Vec::new();

        // Collection reference. A bare variable is referenced directly so the
        // loop's non-consuming reads leave it usable afterward (a scoped
        // borrow); any other expression is a temporary bound once.
        let coll_is_var = matches!(coll_expr, Expr::Ident(_));
        let coll_name: Spur = if let Expr::Ident(id) = coll_expr {
            self.symbol(id.name)
        } else {
            let init = self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), coll_expr);
            let name = self.interner.get_or_intern(format!("__rue_for_coll_{n}"));
            let alloc = self
                .rir
                .add_alloc(&[], Some(name), false, None, init, false, span)
                .record_failure(&mut self.payload_error);
            outer_stmts.push(alloc.as_u32());
            name
        };

        // let mut __p: u64 = 0;   (position — usize is u64)
        let p_name = self.interner.get_or_intern(format!("__rue_for_p_{n}"));
        let u64_sym = self.interner.get_or_intern("u64");
        let zero = self.rir.add_inst(Inst {
            data: InstData::IntConst(0),
            span,
        });
        let p_alloc = self
            .rir
            .add_alloc(&[], Some(p_name), true, Some(u64_sym), zero, false, span)
            .record_failure(&mut self.payload_error);
        outer_stmts.push(p_alloc.as_u32());

        // let __len: u64 = InternalIntrinsic::IterLen(__coll);
        // These two nodes carry the ITERABLE's span, not the whole statement's:
        // Sema's not-iterable type error (E0206) anchors on the intrinsic, and
        // it should underline the offending iterable expression.
        let iter_span = coll_expr.span();
        let len_name = self.interner.get_or_intern(format!("__rue_for_len_{n}"));
        let coll_for_len = self.rir.add_inst(Inst {
            data: InstData::VarRef {
                name: coll_name,
                anchor: None,
            },
            span: iter_span,
        });
        let len_call = self
            .rir
            .add_internal_intrinsic(InternalIntrinsic::IterLen, &[coll_for_len], iter_span)
            .record_failure(&mut self.payload_error);
        let len_alloc = self
            .rir
            .add_alloc(
                &[],
                Some(len_name),
                false,
                Some(u64_sym),
                len_call,
                false,
                span,
            )
            .record_failure(&mut self.payload_error);
        outer_stmts.push(len_alloc.as_u32());

        // ---- loop body ----
        let mut body_stmts: Vec<u32> = Vec::new();

        // if __p >= __len { break }
        let p_ref1 = self.rir.add_inst(Inst {
            data: InstData::VarRef {
                name: p_name,
                anchor: None,
            },
            span,
        });
        let len_ref = self.rir.add_inst(Inst {
            data: InstData::VarRef {
                name: len_name,
                anchor: None,
            },
            span,
        });
        let cond = self.rir.add_inst(Inst {
            data: InstData::Ge {
                lhs: p_ref1,
                rhs: len_ref,
            },
            span,
        });
        let break_inst = self.rir.add_inst(Inst {
            data: InstData::Break { value: None },
            span,
        });
        let end_branch = self.rir.add_inst(Inst {
            data: InstData::Branch {
                cond,
                then_block: break_inst,
                else_block: None,
            },
            span,
        });
        body_stmts.push(end_branch.as_u32());

        // let <binder> = <get>;
        // A `_` binder still binds a (named, underscore-prefixed so it never
        // warns unused) local rather than a discarding `let _`: the element is
        // a shared borrow of the collection (spec 4.8:26), not a value being
        // discarded, so it must NOT go through the discard path — which would
        // drop the borrowed element as a temporary and double-free it (the
        // collection still owns and drops it, RUE-259).
        let binder_name: Option<Spur> = match &for_expr.binder {
            LetPattern::Ident(id) => Some(self.symbol(id.name)),
            LetPattern::Wildcard(_) => {
                Some(self.interner.get_or_intern(format!("_rue_for_elem_{n}")))
            }
        };
        let p_for_get = self.rir.add_inst(Inst {
            data: InstData::VarRef {
                name: p_name,
                anchor: None,
            },
            span,
        });
        let coll_for_get = self.rir.add_inst(Inst {
            data: InstData::VarRef {
                name: coll_name,
                anchor: None,
            },
            span,
        });
        let get_inst = if is_chars {
            let intrinsic = if is_lossy {
                InternalIntrinsic::CharScalarLossy
            } else {
                InternalIntrinsic::CharScalar
            };
            self.rir
                .add_internal_intrinsic(intrinsic, &[coll_for_get, p_for_get], span)
                .record_failure(&mut self.payload_error)
        } else {
            self.rir.add_inst(Inst {
                data: InstData::IndexGet {
                    base: coll_for_get,
                    index: p_for_get,
                },
                span,
            })
        };
        // The element binding is a shared read of the collection (spec 4.8:26):
        // analyzed as a by-ref read so a non-Copy element is not moved out.
        let binder_alloc = self
            .rir
            .add_alloc(&[], binder_name, false, None, get_inst, true, span)
            .record_failure(&mut self.payload_error);
        body_stmts.push(binder_alloc.as_u32());

        // __p = <advance>;   (advanced before the body so `continue` steps)
        let p_for_adv = self.rir.add_inst(Inst {
            data: InstData::VarRef {
                name: p_name,
                anchor: None,
            },
            span,
        });
        let advance = if is_chars {
            let coll_for_adv = self.rir.add_inst(Inst {
                data: InstData::VarRef {
                    name: coll_name,
                    anchor: None,
                },
                span,
            });
            let intrinsic = if is_lossy {
                InternalIntrinsic::CharNextLossy
            } else {
                InternalIntrinsic::CharNext
            };
            self.rir
                .add_internal_intrinsic(intrinsic, &[coll_for_adv, p_for_adv], span)
                .record_failure(&mut self.payload_error)
        } else {
            let one = self.rir.add_inst(Inst {
                data: InstData::IntConst(1),
                span,
            });
            self.rir.add_inst(Inst {
                data: InstData::Add {
                    lhs: p_for_adv,
                    rhs: one,
                },
                span,
            })
        };
        let assign = self.rir.add_inst(Inst {
            data: InstData::Assign {
                name: p_name,
                value: advance,
            },
            span,
        });
        body_stmts.push(assign.as_u32());

        // user body (value discarded)
        let user_body = self
            .with_structural_segment(crate::RirStructuralPathSegment::Branch(0), |this| {
                this.gen_block(&for_expr.body)
            });
        body_stmts.push(user_body.as_u32());

        // block value = ()
        let body_unit = self.rir.add_inst(Inst {
            data: InstData::UnitConst,
            span,
        });
        body_stmts.push(body_unit.as_u32());

        let body_refs: Vec<_> = body_stmts.into_iter().map(InstRef::from_raw).collect();
        let loop_body = self
            .rir
            .add_block(&body_refs, span)
            .record_failure(&mut self.payload_error);

        // A `for` over a named variable holds a scoped shared borrow of that
        // variable for the loop's duration (spec 4.8:26): sema rejects any
        // mutation of it in the body (RUE-233). A `for` over a temporary binds
        // an unnameable local, so there is nothing to borrow-check.
        let iter_borrow = if coll_is_var { Some(coll_name) } else { None };
        let infinite_loop = self.rir.add_inst(Inst {
            data: InstData::InfiniteLoop {
                body: loop_body,
                iter_borrow,
            },
            span,
        });
        outer_stmts.push(infinite_loop.as_u32());

        // outer block value = ()
        let outer_unit = self.rir.add_inst(Inst {
            data: InstData::UnitConst,
            span,
        });
        outer_stmts.push(outer_unit.as_u32());

        let outer_refs: Vec<_> = outer_stmts.into_iter().map(InstRef::from_raw).collect();
        self.rir
            .add_block(&outer_refs, span)
            .record_failure(&mut self.payload_error)
    }

    fn gen_statement(&mut self, stmt: &Statement) -> InstRef {
        match stmt {
            Statement::Let(let_stmt) => {
                let directives = self.convert_directives(&let_stmt.directives);
                let name = match &let_stmt.pattern {
                    LetPattern::Ident(ident) => Some(self.symbol(ident.name)),
                    LetPattern::Wildcard(_) => None,
                };
                let ty = let_stmt
                    .ty
                    .as_ref()
                    .map(|ty| self.intern_type_at(crate::RirStructuralPathSegment::ReturnType, ty));
                let init =
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &let_stmt.init);
                self.rir
                    .add_alloc(
                        &directives,
                        name,
                        let_stmt.is_mut,
                        ty,
                        init,
                        false,
                        let_stmt.span,
                    )
                    .record_failure(&mut self.payload_error)
            }
            Statement::Assign(assign) => {
                let value =
                    self.gen_expr_at(crate::RirStructuralPathSegment::Operand(0), &assign.value);
                match &assign.target {
                    AssignTarget::Var(ident) => self.rir.add_inst(Inst {
                        data: InstData::Assign {
                            name: self.symbol(ident.name),
                            value,
                        },
                        span: assign.span,
                    }),
                    AssignTarget::Field(field_expr) => {
                        let base = self.gen_expr_at(
                            crate::RirStructuralPathSegment::Operand(1),
                            &field_expr.base,
                        );
                        self.rir.add_inst(Inst {
                            data: InstData::FieldSet {
                                base,
                                field: self.symbol(field_expr.field.name),
                                value,
                            },
                            span: assign.span,
                        })
                    }
                    AssignTarget::Index(index_expr) => {
                        let base = self.gen_expr_at(
                            crate::RirStructuralPathSegment::Operand(1),
                            &index_expr.base,
                        );
                        let index = self.gen_expr_at(
                            crate::RirStructuralPathSegment::Operand(2),
                            &index_expr.index,
                        );
                        self.rir.add_inst(Inst {
                            data: InstData::IndexSet { base, index, value },
                            span: assign.span,
                        })
                    }
                }
            }
            Statement::Expr(expr) => {
                // Expression statements are evaluated for side effects
                // The result is discarded, but we still return the InstRef
                self.gen_expr(expr)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inst::RirPrinter;
    use rue_lexer::Lexer;
    use rue_parser::Parser;

    fn gen_rir(source: &str) -> (Rir, ThreadedRodeo) {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        (rir, interner)
    }

    fn anonymous_anchors(source: &str) -> Vec<crate::RirStructuralAnchor> {
        let (rir, _) = gen_rir(source);
        rir.iter()
            .filter_map(|(_, instruction)| match &instruction.data {
                InstData::AnonStructType { anchor, .. } | InstData::AnonEnumType { anchor, .. } => {
                    Some(anchor.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn string_anchors(source: &str) -> Vec<(String, crate::RirStructuralAnchor)> {
        let (rir, interner) = gen_rir(source);
        rir.iter()
            .filter_map(|(_, instruction)| match &instruction.data {
                InstData::StringConst { content, anchor } => {
                    Some((interner.resolve(content).to_owned(), anchor.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn authoritative_anonymous_anchor_install_rejects_span_and_anchor_aliases() {
        let interner = ThreadedRodeo::new();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        let anchor = crate::RirStructuralAnchor::new(vec![crate::RirStructuralPathSegment::Body]);
        let kind = crate::AnonymousTypeSiteKind::Struct;
        let duplicate_span = astgen.install_authoritative_anonymous_anchors([
            (rue_span::Span::new(1, 2), kind, anchor.clone()),
            (
                rue_span::Span::new(1, 2),
                kind,
                crate::RirStructuralAnchor::new(Vec::new()),
            ),
        ]);
        assert!(duplicate_span.is_err());

        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        let duplicate_anchor = astgen.install_authoritative_anonymous_anchors([
            (rue_span::Span::new(1, 2), kind, anchor.clone()),
            (rue_span::Span::new(3, 4), kind, anchor),
        ]);
        assert!(duplicate_anchor.is_err());
    }

    #[test]
    fn string_anchors_are_occurrence_preserving_and_trivia_insensitive() {
        let baseline =
            string_anchors(r#"fn f(comptime b: bool) -> str { if b { "same" } else { "same" } }"#);
        let shifted = string_anchors(
            r#"// unrelated file trivia
               fn unrelated() -> i32 { 0 }
               fn f(comptime b: bool) -> str {
                   if b {
                       // local trivia
                       "same"
                   } else { "same" }
               }"#,
        );
        assert_eq!(baseline, shifted);
        assert_eq!(baseline.len(), 2);
        assert_eq!(baseline[0].0, baseline[1].0);
        assert_ne!(baseline[0].1, baseline[1].1);
        assert!(
            baseline[0]
                .1
                .segments()
                .contains(&crate::RirStructuralPathSegment::Branch(0))
        );
        assert!(
            baseline[1]
                .1
                .segments()
                .contains(&crate::RirStructuralPathSegment::Branch(1))
        );
    }

    #[test]
    fn string_anchor_is_stable_when_an_unrelated_later_statement_is_inserted() {
        let baseline = string_anchors(r#"fn f() -> str { let kept = "kept"; kept }"#);
        let inserted =
            string_anchors(r#"fn f() -> str { let kept = "kept"; let later = "later"; kept }"#);
        assert_eq!(baseline[0], inserted[0]);
        assert_ne!(inserted[0].1, inserted[1].1);
    }

    #[test]
    fn method_string_anchors_reset_at_each_callable_producer() {
        let baseline = string_anchors(r#"struct S { fn keep() -> str { "kept" } }"#);
        let sibling_inserted = string_anchors(
            r#"struct S { fn added() -> str { "added" } fn keep() -> str { "kept" } }"#,
        );
        assert_eq!(baseline[0], sibling_inserted[1]);
        assert_eq!(baseline[0].1, sibling_inserted[0].1);
        assert!(
            baseline[0]
                .1
                .segments()
                .iter()
                .all(|segment| !matches!(segment, crate::RirStructuralPathSegment::Method(_)))
        );
    }

    #[test]
    fn anonymous_anchors_are_trivia_insensitive_and_definition_relative() {
        let baseline = anonymous_anchors("fn make() -> type { struct { x: i32 } }");
        let shifted = anonymous_anchors(
            "// unrelated declaration above the producer\nfn unrelated() -> i32 { 1 }\n\nfn make() -> type {\n    // trivia\n    struct { x: i32 }\n}",
        );
        assert_eq!(baseline, shifted);
        assert_eq!(baseline.len(), 1);
        assert!(
            baseline[0]
                .segments()
                .contains(&crate::RirStructuralPathSegment::Body)
        );
        assert_eq!(
            baseline[0].segments().last(),
            Some(&crate::RirStructuralPathSegment::AnonymousType(0))
        );
    }

    #[test]
    fn anonymous_anchors_distinguish_branches_statements_and_fields() {
        let branches = anonymous_anchors(
            "fn make(comptime b: bool) -> type { if b { struct { x: i32 } } else { struct { x: i32 } } }",
        );
        assert_ne!(branches[0], branches[1]);
        assert!(
            branches[0]
                .segments()
                .contains(&crate::RirStructuralPathSegment::Branch(0))
        );
        assert!(
            branches[1]
                .segments()
                .contains(&crate::RirStructuralPathSegment::Branch(1))
        );

        let statements = anonymous_anchors(
            "fn make() -> type { let A = struct { x: i32 }; let B = struct { x: i32 }; A }",
        );
        assert_ne!(statements[0], statements[1]);
        assert!(
            statements[0]
                .segments()
                .contains(&crate::RirStructuralPathSegment::Statement(0))
        );
        assert!(
            statements[1]
                .segments()
                .contains(&crate::RirStructuralPathSegment::Statement(1))
        );

        let fields = anonymous_anchors(
            "fn make() -> type { Pair { left: struct { x: i32 }, right: struct { x: i32 } } }",
        );
        assert_ne!(fields[0], fields[1]);
        assert!(
            fields[0]
                .segments()
                .contains(&crate::RirStructuralPathSegment::FieldType(0))
        );
        assert!(
            fields[1]
                .segments()
                .contains(&crate::RirStructuralPathSegment::FieldType(1))
        );
    }

    #[test]
    fn method_anchors_are_relative_to_the_method_producer() {
        let baseline =
            anonymous_anchors("struct S { x: i32, fn keep() -> type { struct { x: i32 } } }");
        let sibling_inserted = anonymous_anchors(
            "struct S { x: i32, fn added() -> type { struct { x: i32 } } fn keep() -> type { struct { x: i32 } } }",
        );
        let sibling_reordered = anonymous_anchors(
            "struct S { x: i32, fn keep() -> type { struct { x: i32 } } fn added() -> type { struct { x: i32 } } }",
        );

        assert_eq!(baseline[0], sibling_inserted[1]);
        assert_eq!(baseline[0], sibling_reordered[0]);
        assert_eq!(sibling_inserted[0], sibling_inserted[1]);
        assert!(
            baseline[0]
                .segments()
                .iter()
                .all(|segment| !matches!(segment, crate::RirStructuralPathSegment::Method(_)))
        );
    }

    #[test]
    fn structural_anchor_equality_is_exact_and_segment_framed() {
        use crate::RirStructuralPathSegment as S;
        let path =
            crate::RirStructuralAnchor::new(vec![S::Body, S::Operand(1), S::AnonymousType(0)]);
        assert_eq!(path, path.clone());
        assert_ne!(
            path,
            crate::RirStructuralAnchor::new(vec![S::Body, S::Operand(10), S::AnonymousType(0)])
        );
        assert_ne!(
            crate::RirStructuralAnchor::new(vec![S::Statement(1), S::Operand(2)]),
            crate::RirStructuralAnchor::new(vec![S::Statement(12), S::Operand(0)])
        );
    }

    #[test]
    fn test_gen_simple_function() {
        let (rir, interner) = gen_rir("fn main() -> i32 { 42 }");

        // Should have 2 instructions: IntConst(42), FnDecl
        assert_eq!(rir.len(), 2);

        // Check the function declaration
        let (_, fn_inst) = rir.iter().last().unwrap();
        match &fn_inst.data {
            InstData::FnDecl {
                name,
                params,
                return_type,
                body,
                has_self,
                ..
            } => {
                assert_eq!(interner.resolve(&*name), "main");
                let params = rir.params(params);
                assert_eq!(params.len(), 0);
                assert_eq!(interner.resolve(&*return_type), "i32");
                assert!(!has_self); // Regular functions don't have self
                // Body should be the int constant
                let body_inst = rir.get(*body);
                assert!(matches!(body_inst.data, InstData::IntConst(42)));
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_addition() {
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 }");

        // Should have: IntConst(1), IntConst(2), Add, FnDecl
        assert_eq!(rir.len(), 4);

        // Check add instruction
        let add_inst = rir.get(InstRef::from_raw(2));
        match &add_inst.data {
            InstData::Add { lhs, rhs } => {
                assert!(matches!(rir.get(*lhs).data, InstData::IntConst(1)));
                assert!(matches!(rir.get(*rhs).data, InstData::IntConst(2)));
            }
            _ => panic!("expected Add"),
        }
    }

    #[test]
    fn type_intrinsics_lower_every_type_form_to_canonical_names() {
        // Parity table (RUE-788): the parser hands every type-position
        // intrinsic argument over as `IntrinsicArg::Type`, and lowering
        // interns exactly the canonical spelling annotations produce — the
        // one sema's `resolve_type` consumes.
        for (spelling, canonical) in [
            ("i32", "i32"),
            ("Point", "Point"),
            ("lib.geo.Point", "lib.geo.Point"),
            ("()", "()"),
            ("!", "!"),
            ("[i32; 4]", "[i32; 4]"),
            ("[i32]", "[i32]"),
            ("ptr const i32", "ptr const i32"),
            ("ptr mut ptr const u8", "ptr mut ptr const u8"),
            ("Pair(i32, [i32; 2])", "Pair(i32, [i32; 2])"),
            ("lib.pair.Pair(i32)", "lib.pair.Pair(i32)"),
            // Anonymous `struct`/`enum` literals are no longer valid in a type
            // annotation (RUE-1089 rejects them with E0102 at parse time), so a
            // type-position intrinsic argument can never be one; their rejection
            // is covered by the annotation-position diagnostic tests.
        ] {
            let source = format!("fn f() -> i32 {{ @size_of({spelling}) }}");
            let (rir, interner) = gen_rir(&source);
            let type_arg = rir
                .iter()
                .find_map(|(_, inst)| match &inst.data {
                    InstData::TypeIntrinsic { type_arg, .. } => Some(*type_arg),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no TypeIntrinsic lowered for {spelling}"));
            assert_eq!(interner.resolve(&type_arg), canonical);
        }

        // `@offset_of` routes its type position through the same interning.
        let (rir, interner) = gen_rir("fn f() -> i32 { @offset_of(lib.pair.Pair(i32), second) }");
        let (type_arg, field) = rir
            .iter()
            .find_map(|(_, inst)| match &inst.data {
                InstData::OffsetOf { type_arg, field } => Some((*type_arg, *field)),
                _ => None,
            })
            .expect("no OffsetOf lowered");
        assert_eq!(interner.resolve(&type_arg), "lib.pair.Pair(i32)");
        assert_eq!(interner.resolve(&field), "second");
    }

    #[test]
    fn for_loops_emit_typed_internal_intrinsics() {
        for (iterable, expected) in [
            ("[1, 2]", vec![InternalIntrinsic::IterLen]),
            (
                "\"ok\".chars()",
                vec![
                    InternalIntrinsic::IterLen,
                    InternalIntrinsic::CharScalar,
                    InternalIntrinsic::CharNext,
                ],
            ),
            (
                "\"ok\".chars_lossy()",
                vec![
                    InternalIntrinsic::IterLen,
                    InternalIntrinsic::CharScalarLossy,
                    InternalIntrinsic::CharNextLossy,
                ],
            ),
        ] {
            let source = format!("fn main() {{ for _ in {iterable} {{}} }}");
            let (rir, _) = gen_rir(&source);
            let actual: Vec<_> = rir
                .iter()
                .filter_map(|(_, inst)| match inst.data {
                    InstData::InternalIntrinsic { intrinsic, .. } => Some(intrinsic),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, expected, "{source}");
        }
    }

    #[test]
    fn test_gen_precedence() {
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 * 3 }");

        // Should have: IntConst(1), IntConst(2), IntConst(3), Mul, Add, FnDecl
        assert_eq!(rir.len(), 6);

        // Check that add is the body (mul is nested)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Add { lhs, rhs } => {
                        // lhs should be IntConst(1)
                        assert!(matches!(rir.get(*lhs).data, InstData::IntConst(1)));
                        // rhs should be Mul
                        assert!(matches!(rir.get(*rhs).data, InstData::Mul { .. }));
                    }
                    _ => panic!("expected Add"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_negation() {
        let (rir, _) = gen_rir("fn main() -> i32 { -42 }");

        // Should have: IntConst(42), Neg, FnDecl
        assert_eq!(rir.len(), 3);

        // Check neg instruction
        let neg_inst = rir.get(InstRef::from_raw(1));
        match &neg_inst.data {
            InstData::Neg { operand } => {
                assert!(matches!(rir.get(*operand).data, InstData::IntConst(42)));
            }
            _ => panic!("expected Neg"),
        }
    }

    #[test]
    fn test_gen_parens() {
        let (rir, _) = gen_rir("fn main() -> i32 { (1 + 2) * 3 }");

        // Should have: IntConst(1), IntConst(2), Add, IntConst(3), Mul, FnDecl
        // Parens don't generate instructions, they just affect evaluation order
        assert_eq!(rir.len(), 6);

        // Check that mul is the body (add is nested)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Mul { lhs, rhs } => {
                        // lhs should be Add
                        assert!(matches!(rir.get(*lhs).data, InstData::Add { .. }));
                        // rhs should be IntConst(3)
                        assert!(matches!(rir.get(*rhs).data, InstData::IntConst(3)));
                    }
                    _ => panic!("expected Mul"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_all_binary_ops() {
        // Test all binary operators generate correct instructions
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Add { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 - 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Sub { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 * 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Mul { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 / 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Div { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 % 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Mod { .. }
        ));
    }

    #[test]
    fn test_gen_let_binding() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let x = 42; x }");

        // Find the Alloc instruction
        let alloc_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }));
        assert!(alloc_inst.is_some());

        let (_, inst) = alloc_inst.unwrap();
        match &inst.data {
            InstData::Alloc {
                name,
                is_mut,
                ty,
                init,
                ..
            } => {
                assert_eq!(interner.resolve(&name.unwrap()), "x");
                assert!(!is_mut);
                assert!(ty.is_none());
                assert!(matches!(rir.get(*init).data, InstData::IntConst(42)));
            }
            _ => panic!("expected Alloc"),
        }
    }

    #[test]
    fn test_gen_let_mut() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let mut x = 10; x }");

        let alloc_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }));
        assert!(alloc_inst.is_some());

        let (_, inst) = alloc_inst.unwrap();
        match &inst.data {
            InstData::Alloc { name, is_mut, .. } => {
                assert_eq!(interner.resolve(&name.unwrap()), "x");
                assert!(*is_mut);
            }
            _ => panic!("expected Alloc"),
        }
    }

    #[test]
    fn test_gen_var_ref() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let x = 42; x }");

        // The body should be a Block (since there are statements)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Block { instructions } => {
                        // Block contains: Alloc, VarRef
                        let inst_refs = rir.block_insts(instructions);
                        assert_eq!(inst_refs.len(), 2);
                        // Last instruction in block is the VarRef
                        let var_ref_inst = rir.get(inst_refs.get(1).unwrap());
                        match &var_ref_inst.data {
                            InstData::VarRef { name, .. } => {
                                assert_eq!(interner.resolve(&*name), "x");
                            }
                            _ => panic!("expected VarRef"),
                        }
                    }
                    _ => panic!("expected Block, got {:?}", body_inst.data),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_assignment() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let mut x = 10; x = 20; x }");

        // Find the Assign instruction
        let assign_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Assign { .. }));
        assert!(assign_inst.is_some());

        let (_, inst) = assign_inst.unwrap();
        match &inst.data {
            InstData::Assign { name, value } => {
                assert_eq!(interner.resolve(&*name), "x");
                assert!(matches!(rir.get(*value).data, InstData::IntConst(20)));
            }
            _ => panic!("expected Assign"),
        }
    }

    #[test]
    fn test_gen_multiple_statements() {
        let (rir, _interner) = gen_rir("fn main() -> i32 { let x = 1; let y = 2; x + y }");

        // Count Alloc instructions
        let alloc_count = rir
            .iter()
            .filter(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }))
            .count();
        assert_eq!(alloc_count, 2);

        // Check the body is a Block containing the allocs and the Add
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Block { instructions } => {
                        // Block contains: Alloc(x), Alloc(y), Add
                        let inst_refs = rir.block_insts(instructions);
                        assert_eq!(inst_refs.len(), 3);
                        // Last instruction in block is the Add
                        let add_inst = rir.get(inst_refs.get(2).unwrap());
                        assert!(matches!(add_inst.data, InstData::Add { .. }));
                    }
                    _ => panic!("expected Block"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    // Struct with methods tests
    #[test]
    fn test_gen_struct_with_method() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn get_x(self) -> i32 {
                    self.x
                }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the StructDecl instruction
        let struct_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::StructDecl { .. }));
        assert!(struct_decl.is_some(), "Expected StructDecl instruction");

        let (_, inst) = struct_decl.unwrap();
        match &inst.data {
            InstData::StructDecl { name, methods, .. } => {
                assert_eq!(interner.resolve(&*name), "Point");
                let methods = rir.struct_methods(methods).values().collect::<Vec<_>>();
                assert_eq!(methods.len(), 1);

                // Check the method is a FnDecl with has_self=true
                let method_inst = rir.get(methods[0]);
                match &method_inst.data {
                    InstData::FnDecl { name, has_self, .. } => {
                        assert_eq!(interner.resolve(&*name), "get_x");
                        assert!(*has_self);
                    }
                    _ => panic!("expected FnDecl"),
                }
            }
            _ => panic!("expected StructDecl"),
        }
    }

    #[test]
    fn test_gen_struct_with_multiple_methods() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn get_x(self) -> i32 { self.x }
                fn get_y(self) -> i32 { self.y }
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        let struct_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::StructDecl { .. }));
        assert!(struct_decl.is_some());

        let (_, inst) = struct_decl.unwrap();
        match &inst.data {
            InstData::StructDecl { methods, .. } => {
                let methods = rir.struct_methods(methods);
                assert_eq!(methods.len(), 3);

                // Check get_x and get_y have self, origin does not
                for method_ref in methods {
                    let method_inst = rir.get(method_ref);
                    match &method_inst.data {
                        InstData::FnDecl { name, has_self, .. } => {
                            let method_name = interner.resolve(&*name);
                            if method_name == "origin" {
                                assert!(!has_self, "origin should not have self");
                            } else {
                                assert!(*has_self, "{} should have self", method_name);
                            }
                        }
                        _ => panic!("expected FnDecl"),
                    }
                }
            }
            _ => panic!("expected StructDecl"),
        }
    }

    #[test]
    fn test_gen_method_call() {
        let source = r#"
            struct Point {
                x: i32,
                fn get_x(self) -> i32 { self.x }
            }
            fn main() -> i32 {
                let p = Point { x: 42 };
                p.get_x()
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the MethodCall instruction
        let method_call = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::MethodCall { .. }));
        assert!(method_call.is_some(), "Expected MethodCall instruction");

        let (_, inst) = method_call.unwrap();
        match &inst.data {
            InstData::MethodCall {
                receiver: _,
                method,
                args,
            } => {
                assert_eq!(interner.resolve(&*method), "get_x");
                let args = rir.call_args(args);
                assert_eq!(args.len(), 0); // No explicit args (self is implicit)
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_gen_associated_function_as_method_call() {
        // Associated functions are called with `.` (RUE-488). At the RIR level
        // `Point.origin()` is a `MethodCall` whose receiver is the type name;
        // sema reinterprets it as an associated-function call.
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 {
                let p = Point.origin();
                0
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the MethodCall instruction
        let method_call = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::MethodCall { .. }));
        assert!(method_call.is_some(), "Expected MethodCall instruction");

        let (_, inst) = method_call.unwrap();
        match &inst.data {
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                match &rir.get(*receiver).data {
                    InstData::VarRef { name, .. } => assert_eq!(interner.resolve(name), "Point"),
                    other => panic!("expected VarRef receiver, got {other:?}"),
                }
                assert_eq!(interner.resolve(&*method), "origin");
                let args = rir.call_args(args);
                assert_eq!(args.len(), 0);
            }
            _ => panic!("expected MethodCall"),
        }
    }

    // Pattern tests
    #[test]
    fn test_gen_match_wildcard_pattern() {
        let source = r#"
            fn main() -> i32 {
                let x = 5;
                match x {
                    _ => 42,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        // Find the Match instruction
        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some(), "Expected Match instruction");

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match { arms, .. } => {
                let arms: Vec<_> = rir
                    .match_arms(arms)
                    .iter()
                    .map(|(pattern, body)| (pattern.to_owned(), body))
                    .collect();
                assert_eq!(arms.len(), 1);
                assert!(matches!(arms[0].0, RirPattern::Wildcard(_)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_int_patterns() {
        let source = r#"
            fn main() -> i32 {
                let x = 5;
                match x {
                    1 => 10,
                    2 => 20,
                    _ => 0,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match { arms, .. } => {
                let arms: Vec<_> = rir
                    .match_arms(arms)
                    .iter()
                    .map(|(pattern, body)| (pattern.to_owned(), body))
                    .collect();
                assert_eq!(arms.len(), 3);
                assert!(matches!(
                    arms[0].0,
                    RirPattern::Int {
                        value: 1,
                        negative: false,
                        ..
                    }
                ));
                assert!(matches!(
                    arms[1].0,
                    RirPattern::Int {
                        value: 2,
                        negative: false,
                        ..
                    }
                ));
                assert!(matches!(arms[2].0, RirPattern::Wildcard(_)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_negative_int_pattern() {
        let source = r#"
            fn main() -> i32 {
                let x: i32 = -5;
                match x {
                    -5 => 1,
                    -10 => 2,
                    _ => 0,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match { arms, .. } => {
                let arms: Vec<_> = rir
                    .match_arms(arms)
                    .iter()
                    .map(|(pattern, body)| (pattern.to_owned(), body))
                    .collect();
                assert_eq!(arms.len(), 3);
                assert!(matches!(
                    arms[0].0,
                    RirPattern::Int {
                        value: 5,
                        negative: true,
                        ..
                    }
                ));
                assert!(matches!(
                    arms[1].0,
                    RirPattern::Int {
                        value: 10,
                        negative: true,
                        ..
                    }
                ));
                assert!(matches!(arms[2].0, RirPattern::Wildcard(_)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_bool_patterns() {
        let source = r#"
            fn main() -> i32 {
                let b = true;
                match b {
                    true => 1,
                    false => 0,
                }
            }
        "#;
        let (rir, _interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match { arms, .. } => {
                let arms: Vec<_> = rir
                    .match_arms(arms)
                    .iter()
                    .map(|(pattern, body)| (pattern.to_owned(), body))
                    .collect();
                assert_eq!(arms.len(), 2);
                assert!(matches!(arms[0].0, RirPattern::Bool(true, _)));
                assert!(matches!(arms[1].0, RirPattern::Bool(false, _)));
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_match_enum_patterns() {
        let source = r#"
            enum Color { Red, Green, Blue }
            fn main() -> i32 {
                let c = Color.Red;
                match c {
                    Color.Red => 1,
                    Color.Green => 2,
                    Color.Blue => 3,
                }
            }
        "#;
        let (rir, interner) = gen_rir(source);

        let match_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Match { .. }));
        assert!(match_inst.is_some());

        let (_, inst) = match_inst.unwrap();
        match &inst.data {
            InstData::Match { arms, .. } => {
                let arms: Vec<_> = rir
                    .match_arms(arms)
                    .iter()
                    .map(|(pattern, body)| (pattern.to_owned(), body))
                    .collect();
                assert_eq!(arms.len(), 3);

                // Check first arm is Color.Red
                match &arms[0].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(type_name), "Color");
                        assert_eq!(interner.resolve(variant), "Red");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check second arm is Color.Green
                match &arms[1].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(type_name), "Color");
                        assert_eq!(interner.resolve(variant), "Green");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check third arm is Color.Blue
                match &arms[2].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(type_name), "Color");
                        assert_eq!(interner.resolve(variant), "Blue");
                    }
                    _ => panic!("expected Path pattern"),
                }
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_gen_self_expr() {
        let source = r#"
            struct Point {
                x: i32,
                fn get_x(self) -> i32 { self.x }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the VarRef instruction for "self"
        let self_ref = rir.iter().find(|(_, inst)| match &inst.data {
            InstData::VarRef { name, .. } => interner.resolve(&*name) == "self",
            _ => false,
        });
        assert!(self_ref.is_some(), "Expected self VarRef instruction");
    }

    #[test]
    fn test_gen_drop_fn() {
        let source = r#"
            struct Resource { value: i32 }
            drop fn Resource(self) { () }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the DropFnDecl instruction
        let drop_fn = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::DropFnDecl { .. }));
        assert!(drop_fn.is_some(), "Expected DropFnDecl instruction");

        let (_, inst) = drop_fn.unwrap();
        match &inst.data {
            InstData::DropFnDecl { type_name, body: _ } => {
                assert_eq!(interner.resolve(&*type_name), "Resource");
            }
            _ => panic!("expected DropFnDecl"),
        }
    }

    #[test]
    fn test_gen_enum_variant() {
        // Enum variants are spelled with `.` (RUE-488). At the RIR level
        // `Color.Red` is a `FieldGet` on the type name; sema reinterprets it as
        // an enum-variant value.
        let source = r#"
            enum Color { Red, Green, Blue }
            fn main() -> i32 {
                let c = Color.Red;
                0
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the FieldGet instruction
        let field_get = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::FieldGet { .. }));
        assert!(field_get.is_some(), "Expected FieldGet instruction");

        let (_, inst) = field_get.unwrap();
        match &inst.data {
            InstData::FieldGet { base, field } => {
                match &rir.get(*base).data {
                    InstData::VarRef { name, .. } => assert_eq!(interner.resolve(name), "Color"),
                    other => panic!("expected VarRef base, got {other:?}"),
                }
                assert_eq!(interner.resolve(&*field), "Red");
            }
            _ => panic!("expected FieldGet"),
        }
    }

    #[test]
    fn test_gen_method_with_params() {
        let source = r#"
            struct Counter {
                value: i32,
                fn add(self, amount: i32) -> i32 { self.value + amount }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the struct declaration
        let struct_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::StructDecl { .. }));
        assert!(struct_decl.is_some());

        let (_, inst) = struct_decl.unwrap();
        match &inst.data {
            InstData::StructDecl { methods, .. } => {
                let methods = rir.struct_methods(methods).values().collect::<Vec<_>>();
                let method_inst = rir.get(methods[0]);
                match &method_inst.data {
                    InstData::FnDecl {
                        name,
                        params,
                        has_self,
                        ..
                    } => {
                        assert_eq!(interner.resolve(&*name), "add");
                        assert!(*has_self);
                        // params should contain 'amount', not 'self'
                        let params = rir.params(params).to_vec();
                        assert_eq!(params.len(), 1);
                        assert_eq!(interner.resolve(&params[0].name), "amount");
                    }
                    _ => panic!("expected FnDecl"),
                }
            }
            _ => panic!("expected StructDecl"),
        }
    }

    // RirPrinter integration test with actual generated RIR
    #[test]
    fn test_printer_integration() {
        let source = r#"
            struct Point {
                x: i32,
                y: i32,
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 {
                let p = Point.origin();
                p.x
            }
        "#;
        let (rir, interner) = gen_rir(source);

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();

        // Check key elements are present in the output. `Point.origin()` lowers
        // to a `method_call` at the RIR level (RUE-488); sema reinterprets the
        // type-name receiver as an associated-function call.
        assert!(output.contains("struct Point"));
        assert!(output.contains("methods: ["));
        assert!(output.contains("fn origin"));
        assert!(output.contains("fn main"));
        assert!(output.contains("struct_init Point"));
        assert!(output.contains("method_call"));
        assert!(output.contains("field_get"));
    }

    #[test]
    fn test_anon_struct_with_methods() {
        // Test that anonymous structs with methods generate AnonStructType with method references
        let source = r#"
            fn MakePoint(comptime T: type) -> type {
                struct {
                    x: T,
                    y: T,

                    fn get_x(self) -> T { self.x }
                    fn origin() -> Self { Self { x: 0, y: 0 } }
                }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the AnonStructType instruction
        let anon_struct = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::AnonStructType { .. }));
        assert!(
            anon_struct.is_some(),
            "Expected to find AnonStructType instruction"
        );

        let (_, inst) = anon_struct.unwrap();
        match &inst.data {
            InstData::AnonStructType {
                fields, methods, ..
            } => {
                // Should have 2 fields (x and y)
                let fields = rir.anon_struct_fields(fields).to_vec();
                assert_eq!(fields.len(), 2);
                assert_eq!(interner.resolve(&fields[0].0), "x");
                assert_eq!(interner.resolve(&fields[1].0), "y");

                // Should have 2 methods (get_x and origin)
                let methods = rir.anon_struct_methods(methods);
                assert_eq!(methods.len(), 2);

                // Verify each method is a FnDecl
                for method_ref in methods {
                    let method_inst = rir.get(method_ref);
                    match &method_inst.data {
                        InstData::FnDecl { name, has_self, .. } => {
                            let name_str = interner.resolve(name);
                            // get_x has self, origin doesn't
                            if name_str == "get_x" {
                                assert!(*has_self, "get_x should have self parameter");
                            } else if name_str == "origin" {
                                assert!(!*has_self, "origin should not have self parameter");
                            }
                        }
                        _ => panic!("Expected FnDecl for method"),
                    }
                }
            }
            _ => panic!("Expected AnonStructType"),
        }
    }

    #[test]
    fn test_anon_struct_without_methods() {
        // Test that anonymous structs without methods have zero methods_len
        let source = r#"
            fn MakePair(comptime T: type) -> type {
                struct { first: T, second: T }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, _interner) = gen_rir(source);

        // Find the AnonStructType instruction
        let anon_struct = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::AnonStructType { .. }));
        assert!(
            anon_struct.is_some(),
            "Expected to find AnonStructType instruction"
        );

        let (_, inst) = anon_struct.unwrap();
        match &inst.data {
            InstData::AnonStructType { methods, .. } => {
                assert!(
                    rir.anon_struct_methods(methods).is_empty(),
                    "Expected no methods"
                );
            }
            _ => panic!("Expected AnonStructType"),
        }
    }
}
