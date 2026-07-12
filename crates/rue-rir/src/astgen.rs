//! AST to RIR generation.
//!
//! AstGen converts the Abstract Syntax Tree into RIR instructions.
//! This is analogous to Zig's AstGen phase.

use lasso::{Key, Spur, ThreadedRodeo};

/// Known type intrinsics that take a type argument rather than an expression.
/// These intrinsics operate on types at compile time (e.g., @size_of(i32)).
///
/// `require_droppable` is the compile-time well-formedness gate an owning
/// growable container (`std/arraybuf.rue`'s `ArrayBuf(T)`) uses to reject an
/// element type it cannot yet correctly own — one with a destructor or a
/// `linear` element (RUE-388). It takes the element type as its sole argument
/// and evaluates to unit during comptime type-constructor reduction, so it must
/// be lowered as a `TypeIntrinsic` (like `@size_of`), not an expression call.
const TYPE_INTRINSICS: &[&str] = &[
    "size_of",
    "align_of",
    "require_droppable",
    "require_trivially_droppable",
];
use rue_parser::ast::{ConstDecl, DropFn};
use rue_parser::{
    ArgMode, ArrayLength, AssignTarget, Ast, BinaryOp, CallArg, Directive, DirectiveArg, EnumDecl,
    Expr, Function, IntrinsicArg, Item, LetPattern, Method, ParamMode, Pattern, Statement,
    StructDecl, TypeExpr, UnaryOp, ast::Visibility,
};

use crate::inst::{
    Inst, InstData, InstRef, RepeatCount, Rir, RirArgMode, RirCallArg, RirDirective, RirParam,
    RirParamMode, RirPattern,
};

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// The AST being processed by the compatibility constructor.
    ast: Option<&'a Ast>,
    /// String interner for symbols (thread-safe, takes shared reference)
    interner: &'a ThreadedRodeo,
    /// Output RIR
    rir: Rir,
    /// Monotonic counter used to mint unique names for the compiler-generated
    /// temporaries of a `for`-loop desugaring (RUE-220), so nested for-loops
    /// don't shadow one another's position/length/collection bindings.
    for_counter: u32,
    normalize_symbol: Box<dyn Fn(Spur) -> Spur + 'a>,
}

impl<'a> AstGen<'a> {
    /// Create a new AstGen for the given AST.
    pub fn new(ast: &'a Ast, interner: &'a ThreadedRodeo) -> Self {
        Self {
            ast: Some(ast),
            interner,
            rir: Rir::new(),
            for_counter: 0,
            normalize_symbol: Box::new(|symbol| symbol),
        }
    }

    /// Generate RIR from the AST.
    pub fn generate(mut self) -> Rir {
        let ast = self.ast.expect("AstGen::new always stores an AST");
        for item in &ast.items {
            self.gen_item(item);
        }
        self.rir
    }

    /// Generate RIR from an immutable borrowed sequence of AST items.
    ///
    /// The iterator order is the lowering order. This lets compiler
    /// orchestration supply a canonical query/view without cloning item
    /// payloads or constructing a second [`Ast`].
    pub fn generate_items<'item>(
        interner: &'a ThreadedRodeo,
        items: impl IntoIterator<Item = &'item Item>,
    ) -> Rir {
        let mut generator = Self {
            ast: None,
            interner,
            rir: Rir::new(),
            for_counter: 0,
            normalize_symbol: Box::new(|symbol| symbol),
        };
        for item in items {
            generator.gen_item(item);
        }
        generator.rir
    }

    /// Create a generator whose AST-origin symbols are normalized before use.
    #[doc(hidden)]
    pub fn with_symbol_normalizer(
        interner: &'a ThreadedRodeo,
        normalize_symbol: impl Fn(Spur) -> Spur + 'a,
    ) -> Self {
        Self {
            ast: None,
            interner,
            rir: Rir::new(),
            for_counter: 0,
            normalize_symbol: Box::new(normalize_symbol),
        }
    }

    /// Append borrowed items while preserving generator-global state.
    #[doc(hidden)]
    pub fn append_items<'item>(&mut self, items: impl IntoIterator<Item = &'item Item>) {
        for item in items {
            self.gen_item(item);
        }
    }

    /// Finish a normalized multi-module lowering session.
    #[doc(hidden)]
    pub fn finish(self) -> Rir {
        self.rir
    }

    fn symbol(&self, symbol: Spur) -> Spur {
        (self.normalize_symbol)(symbol)
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
        let (directives_start, directives_len) = self.rir.add_directives(&directives);
        let name = self.symbol(struct_decl.name.name);
        let fields: Vec<_> = struct_decl
            .fields
            .iter()
            .map(|f| {
                let field_name = self.symbol(f.name.name);
                let field_type = self.intern_type(&f.ty);
                (field_name, field_type)
            })
            .collect();
        let (fields_start, fields_len) = self.rir.add_field_decls(&fields);

        // Generate each method defined inline in the struct
        let methods: Vec<_> = struct_decl
            .methods
            .iter()
            .map(|m| self.gen_method(m))
            .collect();
        let (methods_start, methods_len) = self.rir.add_inst_refs(&methods);

        self.rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: struct_decl.visibility == Visibility::Public,
                is_linear: struct_decl.is_linear,
                name,
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            },
            span: struct_decl.span,
        })
    }

    fn gen_enum(&mut self, enum_decl: &EnumDecl) -> InstRef {
        let name = self.symbol(enum_decl.name.name);
        let variants: Vec<_> = enum_decl
            .variants
            .iter()
            .map(|v| self.symbol(v.name.name))
            .collect();
        let (variants_start, variants_len) = self.rir.add_symbols(&variants);

        // Encode tuple-variant payloads (RUE-221) as a self-describing flat
        // sequence: for each variant, a count `k` followed by `k` payload
        // type-name symbols. Discriminant-only variants contribute a `0`.
        // The whole region is omitted (len 0) when no variant carries data.
        let has_any_payload = enum_decl.variants.iter().any(|v| !v.payload.is_empty());
        let (payloads_start, payloads_len) = if has_any_payload {
            let mut payload_words: Vec<u32> = Vec::new();
            for variant in &enum_decl.variants {
                payload_words.push(variant.payload.len() as u32);
                for ty in &variant.payload {
                    let ty_sym = self.intern_type(ty);
                    payload_words.push(ty_sym.into_usize() as u32);
                }
            }
            let start = self.rir.add_extra(&payload_words);
            (start, payload_words.len() as u32)
        } else {
            (0, 0)
        };

        self.rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: enum_decl.visibility == Visibility::Public,
                name,
                variants_start,
                variants_len,
                payloads_start,
                payloads_len,
            },
            span: enum_decl.span,
        })
    }

    fn gen_const(&mut self, const_decl: &ConstDecl) -> InstRef {
        let directives = self.convert_directives(&const_decl.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);
        let name = self.symbol(const_decl.name.name);
        let ty = const_decl.ty.as_ref().map(|t| self.intern_type(t));
        let init = self.gen_expr(&const_decl.init);

        self.rir.add_inst(Inst {
            data: InstData::ConstDecl {
                directives_start,
                directives_len,
                is_pub: const_decl.visibility == Visibility::Public,
                name,
                ty,
                init,
            },
            span: const_decl.span,
        })
    }

    fn gen_drop_fn(&mut self, drop_fn: &DropFn) -> InstRef {
        let type_name = self.symbol(drop_fn.type_name.name);

        // Generate the body expression
        let body = self.gen_expr(&drop_fn.body);

        self.rir.add_inst(Inst {
            data: InstData::DropFnDecl { type_name, body },
            span: drop_fn.span,
        })
    }

    fn gen_method(&mut self, method: &Method) -> InstRef {
        // Convert directives
        let directives = self.convert_directives(&method.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);

        // Get the method name (already a Symbol) and return type
        let name = self.symbol(method.name.name);
        let return_type = match &method.return_type {
            Some(ty) => self.intern_type(ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters (excluding self, which is handled specially by sema)
        let params: Vec<_> = method
            .params
            .iter()
            .map(|p| RirParam {
                name: self.symbol(p.name.name),
                ty: self.intern_type(&p.ty),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.mode == ParamMode::Comptime,
                span: p.name.span,
            })
            .collect();
        let (params_start, params_len) = self.rir.add_params(&params);

        // Generate body expression
        let body = self.gen_expr(&method.body);

        // Track whether this method has a self receiver (method vs associated
        // function) and, if so, the receiver's passing mode (`borrow self` /
        // `inout self` / bare by-value `self`, RUE-15).
        let has_self = method.receiver.is_some();
        let self_mode = match &method.receiver {
            Some(receiver) => self.convert_param_mode(receiver.mode),
            None => RirParamMode::Normal,
        };

        // Emit methods as FnDecl instructions with has_self flag.
        // Sema uses has_self to add the implicit self parameter for methods,
        // and self_mode to add it in the declared borrow/inout/by-value mode.
        // Methods don't have their own visibility - they're accessible if the type is accessible.
        // Methods cannot be marked unchecked (that's a function-level modifier).
        let decl = self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                is_unchecked: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                self_mode,
            },
            span: method.span,
        });

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
    /// The AST has a single `ParamMode` (RUE-133 collapsed the old
    /// `is_comptime` flag into it), but the RIR keeps `mode` and
    /// `is_comptime` as separate fields. A comptime parameter lowers to
    /// `mode: Normal, is_comptime: true` — exactly the RIR shape sema has
    /// always consumed — so `RirParamMode::Comptime` is never constructed
    /// here.
    fn convert_param_mode(&self, mode: ParamMode) -> RirParamMode {
        match mode {
            ParamMode::Normal | ParamMode::Comptime => RirParamMode::Normal,
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
        // Convert directives
        let directives = self.convert_directives(&func.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);

        // Get the function name (already a Symbol) and return type
        let name = self.symbol(func.name.name);
        let return_type = match &func.return_type {
            Some(ty) => self.intern_type(ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters
        let params: Vec<_> = func
            .params
            .iter()
            .map(|p| RirParam {
                name: self.symbol(p.name.name),
                ty: self.intern_type(&p.ty),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.mode == ParamMode::Comptime,
                span: p.name.span,
            })
            .collect();
        let (params_start, params_len) = self.rir.add_params(&params);

        // Generate body expression
        let body = self.gen_expr(&func.body);

        // Create function declaration instruction
        // Regular functions don't have a self receiver
        let decl = self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: func.visibility == Visibility::Public,
                is_unchecked: func.is_unchecked,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
                self_mode: RirParamMode::Normal,
            },
            span: func.span,
        });

        decl
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
                data: InstData::StringConst(self.symbol(lit.value)),
                span: lit.span,
            }),
            Expr::Unit(lit) => self.rir.add_inst(Inst {
                data: InstData::UnitConst,
                span: lit.span,
            }),
            Expr::Ident(ident) => self.rir.add_inst(Inst {
                data: InstData::VarRef {
                    name: self.symbol(ident.name),
                },
                span: ident.span,
            }),
            Expr::Binary(bin) => {
                let lhs = self.gen_expr(&bin.left);
                let rhs = self.gen_expr(&bin.right);
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
                let operand = self.gen_expr(&un.operand);
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
                let operand = self.gen_expr(&try_expr.operand);
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
                let cond = self.gen_expr(&if_expr.cond);
                let then_block = self.gen_block(&if_expr.then_block);
                let else_block = if_expr.else_block.as_ref().map(|b| self.gen_block(b));

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
                let cond = self.gen_expr(&while_expr.cond);
                let body = self.gen_block(&while_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::Loop { cond, body },
                    span: while_expr.span,
                })
            }
            Expr::For(for_expr) => self.gen_for(for_expr),
            Expr::Loop(loop_expr) => {
                let body = self.gen_block(&loop_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::InfiniteLoop {
                        body,
                        iter_borrow: None,
                    },
                    span: loop_expr.span,
                })
            }
            Expr::Match(match_expr) => {
                let scrutinee = self.gen_expr(&match_expr.scrutinee);
                let arms: Vec<_> = match_expr
                    .arms
                    .iter()
                    .map(|arm| {
                        let pattern = self.gen_pattern(&arm.pattern);
                        let body = self.gen_expr(&arm.body);
                        (pattern, body)
                    })
                    .collect();
                let (arms_start, arms_len) = self.rir.add_match_arms(&arms);

                self.rir.add_inst(Inst {
                    data: InstData::Match {
                        scrutinee,
                        arms_start,
                        arms_len,
                    },
                    span: match_expr.span,
                })
            }
            Expr::Call(call) => {
                let args: Vec<_> = call.args.iter().map(|a| self.convert_call_arg(a)).collect();
                let (args_start, args_len) = self.rir.add_call_args(&args);

                self.rir.add_inst(Inst {
                    data: InstData::Call {
                        name: self.symbol(call.name.name),
                        args_start,
                        args_len,
                    },
                    span: call.span,
                })
            }
            Expr::Break(break_expr) => {
                let value = break_expr.value.as_ref().map(|v| self.gen_expr(v));
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
                let value = return_expr.value.as_ref().map(|v| self.gen_expr(v));
                self.rir.add_inst(Inst {
                    data: InstData::Ret(value),
                    span: return_expr.span,
                })
            }
            Expr::StructLit(struct_lit) => {
                // Generate module reference if this is a qualified struct literal
                let module = struct_lit
                    .base
                    .as_ref()
                    .map(|base_expr| self.gen_expr(base_expr));

                // Inline type-constructor struct-literal head `F(args) { ... }`
                // (RUE-596): generate the constructor call `F(args)` as its own
                // instruction; sema reduces it to the struct type at comptime.
                let ctor_head = struct_lit.ctor_args.as_ref().map(|args| {
                    let arg_refs: Vec<_> = args.iter().map(|a| self.convert_call_arg(a)).collect();
                    let (args_start, args_len) = self.rir.add_call_args(&arg_refs);
                    self.rir.add_inst(Inst {
                        data: InstData::Call {
                            name: self.symbol(struct_lit.name.name),
                            args_start,
                            args_len,
                        },
                        span: struct_lit.span,
                    })
                });

                let fields: Vec<_> = struct_lit
                    .fields
                    .iter()
                    .map(|f| {
                        let field_value = self.gen_expr(&f.value);
                        (self.symbol(f.name.name), field_value)
                    })
                    .collect();
                let (fields_start, fields_len) = self.rir.add_field_inits(&fields);

                // Field-init shorthand (`P { x }`, RUE-613) is fully desugared to
                // `x: x` above; carry the first shorthand field's span so Sema can
                // gate the form behind its preview flag.
                let shorthand_span = struct_lit
                    .fields
                    .iter()
                    .find(|f| f.shorthand)
                    .map(|f| f.span);

                self.rir.add_inst(Inst {
                    data: InstData::StructInit {
                        module,
                        ctor_head,
                        type_name: self.symbol(struct_lit.name.name),
                        fields_start,
                        fields_len,
                        shorthand_span,
                    },
                    span: struct_lit.span,
                })
            }
            Expr::Field(field_expr) => {
                let base = self.gen_expr(&field_expr.base);

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
                // addressing: the first argument names a struct type and the
                // second names one of its fields. Both spell as bare
                // identifiers (`Point`, `x`) — the parser hands them over as
                // `Expr::Ident` (or the first as a `Type` when it is an
                // unambiguous type form). Lower the pair into a dedicated
                // `OffsetOf` node so Sema can compute the offset from the
                // layout it assigns, rather than the user hardcoding a literal.
                if intrinsic_name_str == "offset_of" && intrinsic.args.len() == 2 {
                    let type_arg = match &intrinsic.args[0] {
                        IntrinsicArg::Type(ty) => Some(self.intern_type(ty)),
                        IntrinsicArg::Expr(Expr::Ident(ident)) => Some(self.symbol(ident.name)),
                        _ => None,
                    };
                    if let (Some(type_arg), IntrinsicArg::Expr(Expr::Ident(field))) =
                        (type_arg, &intrinsic.args[1])
                    {
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

                let is_type_intrinsic = TYPE_INTRINSICS.contains(&intrinsic_name_str);

                if is_type_intrinsic && intrinsic.args.len() == 1 {
                    // Handle explicit type argument
                    if let IntrinsicArg::Type(ty) = &intrinsic.args[0] {
                        let type_arg = self.intern_type(ty);
                        return self.rir.add_inst(Inst {
                            data: InstData::TypeIntrinsic { name, type_arg },
                            span: intrinsic.span,
                        });
                    }

                    // Handle identifier expression that should be interpreted as a type
                    // (e.g., @size_of(Point) where Point is parsed as Ident expression)
                    if let IntrinsicArg::Expr(Expr::Ident(ident)) = &intrinsic.args[0] {
                        return self.rir.add_inst(Inst {
                            data: InstData::TypeIntrinsic {
                                name,
                                type_arg: self.symbol(ident.name),
                            },
                            span: intrinsic.span,
                        });
                    }
                }

                // Otherwise, treat as an expression intrinsic
                let args: Vec<_> = intrinsic
                    .args
                    .iter()
                    .map(|a| match a {
                        IntrinsicArg::Expr(expr) => self.gen_expr(expr),
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
                let (args_start, args_len) = self.rir.add_inst_refs(&args);

                self.rir.add_inst(Inst {
                    data: InstData::Intrinsic {
                        name,
                        args_start,
                        args_len,
                    },
                    span: intrinsic.span,
                })
            }
            Expr::ArrayLit(array_lit) => {
                if let Some(count) = &array_lit.repeat {
                    // Repeat form `[value; count]` (RUE-235): the single value
                    // is evaluated once; the count is carried symbolically and
                    // resolved during sema via the array-length const-eval path.
                    let value = self.gen_expr(&array_lit.elements[0]);
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
                        .map(|e| self.gen_expr(e))
                        .collect();
                    let (elems_start, elems_len) = self.rir.add_inst_refs(&elements);

                    self.rir.add_inst(Inst {
                        data: InstData::ArrayInit {
                            elems_start,
                            elems_len,
                        },
                        span: array_lit.span,
                    })
                }
            }
            Expr::Index(index_expr) => {
                let base = self.gen_expr(&index_expr.base);
                let index = self.gen_expr(&index_expr.index);

                self.rir.add_inst(Inst {
                    data: InstData::IndexGet { base, index },
                    span: index_expr.span,
                })
            }
            Expr::Path(path_expr) => {
                // Generate module reference if this is a qualified path
                let module = path_expr
                    .base
                    .as_ref()
                    .map(|base_expr| self.gen_expr(base_expr));

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
                let receiver = self.gen_expr(&method_call.receiver);
                let args: Vec<_> = method_call
                    .args
                    .iter()
                    .map(|a| self.convert_call_arg(a))
                    .collect();
                let (args_start, args_len) = self.rir.add_call_args(&args);

                self.rir.add_inst(Inst {
                    data: InstData::MethodCall {
                        receiver,
                        method: self.symbol(method_call.method.name),
                        args_start,
                        args_len,
                    },
                    span: method_call.span,
                })
            }
            Expr::AssocFnCall(assoc_fn_call) => {
                let args: Vec<_> = assoc_fn_call
                    .args
                    .iter()
                    .map(|a| self.convert_call_arg(a))
                    .collect();
                let (args_start, args_len) = self.rir.add_call_args(&args);

                self.rir.add_inst(Inst {
                    data: InstData::AssocFnCall {
                        type_name: self.symbol(assoc_fn_call.type_name.name),
                        function: self.symbol(assoc_fn_call.function.name),
                        args_start,
                        args_len,
                    },
                    span: assoc_fn_call.span,
                })
            }
            Expr::SelfExpr(self_expr) => {
                // `self` in method bodies is just a variable reference to the implicit self parameter
                let name = self.interner.get_or_intern("self");
                self.rir.add_inst(Inst {
                    data: InstData::VarRef { name },
                    span: self_expr.span,
                })
            }
            Expr::Comptime(comptime_block) => {
                // Generate the inner expression, wrapped in a Comptime instruction
                // The semantic analyzer will evaluate this at compile time
                let inner_expr = self.gen_expr(&comptime_block.expr);
                self.rir.add_inst(Inst {
                    data: InstData::Comptime { expr: inner_expr },
                    span: comptime_block.span,
                })
            }
            Expr::Checked(checked_block) => {
                // Generate the inner expression, wrapped in a Checked instruction
                // Unchecked operations are only allowed inside checked blocks
                let inner_expr = self.gen_expr(&checked_block.expr);
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
                        // Generate an anonymous struct type instruction with methods
                        let field_decls: Vec<(Spur, Spur)> = fields
                            .iter()
                            .map(|f| {
                                let name = self.symbol(f.name.name);
                                let ty = self.intern_type(&f.ty);
                                (name, ty)
                            })
                            .collect();
                        let (fields_start, fields_len) = self.rir.add_field_decls(&field_decls);

                        // Generate each method inside the anonymous struct
                        // (reusing gen_method, which generates FnDecl instructions)
                        let method_refs: Vec<InstRef> =
                            methods.iter().map(|m| self.gen_method(m)).collect();
                        let (methods_start, methods_len) = self.rir.add_inst_refs(&method_refs);

                        self.rir.add_inst(Inst {
                            data: InstData::AnonStructType {
                                fields_start,
                                fields_len,
                                methods_start,
                                methods_len,
                            },
                            span: type_lit.span,
                        })
                    }
                    TypeExpr::AnonymousEnum { variants, .. } => {
                        // Generate an anonymous enum type instruction. Variant
                        // names and tuple-variant payloads are encoded exactly
                        // as `gen_enum` does for a top-level `enum` declaration
                        // (RUE-221, ADR-0038).
                        let variant_syms: Vec<Spur> =
                            variants.iter().map(|v| self.symbol(v.name.name)).collect();
                        let (variants_start, variants_len) = self.rir.add_symbols(&variant_syms);

                        let has_any_payload = variants.iter().any(|v| !v.payload.is_empty());
                        let (payloads_start, payloads_len) = if has_any_payload {
                            let mut payload_words: Vec<u32> = Vec::new();
                            for variant in variants {
                                payload_words.push(variant.payload.len() as u32);
                                for ty in &variant.payload {
                                    let ty_sym = self.intern_type(ty);
                                    payload_words.push(ty_sym.into_usize() as u32);
                                }
                            }
                            let start = self.rir.add_extra(&payload_words);
                            (start, payload_words.len() as u32)
                        } else {
                            (0, 0)
                        };

                        self.rir.add_inst(Inst {
                            data: InstData::AnonEnumType {
                                variants_start,
                                variants_len,
                                payloads_start,
                                payloads_len,
                            },
                            span: type_lit.span,
                        })
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
                let module = path.base.as_ref().map(|base| self.gen_expr(base));
                // Inline type-constructor pattern head `F(args).Variant(..)`
                // (RUE-596): generate the constructor call `F(args)` as its own
                // instruction; sema reduces it to the enum type at comptime.
                let ctor_head = path.ctor_args.as_ref().map(|args| {
                    let arg_refs: Vec<_> = args.iter().map(|a| self.convert_call_arg(a)).collect();
                    let (args_start, args_len) = self.rir.add_call_args(&arg_refs);
                    self.rir.add_inst(Inst {
                        data: InstData::Call {
                            name: self.symbol(path.type_name.name),
                            args_start,
                            args_len,
                        },
                        span: path.span,
                    })
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
        if block.statements.is_empty() {
            // No statements, just the final expression
            self.gen_expr(&block.expr)
        } else {
            // Collect all instruction refs for the block
            // statements + 1 for the final expression
            let mut inst_refs = Vec::with_capacity(block.statements.len() + 1);

            // Generate all statements first
            for stmt in &block.statements {
                let inst_ref = self.gen_statement(stmt);
                inst_refs.push(inst_ref.as_u32());
            }

            // Generate the final expression
            let final_expr = self.gen_expr(&block.expr);
            inst_refs.push(final_expr.as_u32());

            // Store the refs in extra data
            let extra_start = self.rir.add_extra(&inst_refs);
            let len = inst_refs.len() as u32;

            self.rir.add_inst(Inst {
                data: InstData::Block { extra_start, len },
                span: block.span,
            })
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
    /// The type-dependent pieces are three compiler-internal intrinsics that
    /// Sema resolves by the collection's type (dispatching the three iterable
    /// kinds — array, String byte view, String `.chars()` / `.chars_lossy()`
    /// scalar views): `@__rue_iter_len`, and for the char views
    /// `@__rue_char_scalar` / `@__rue_char_next` (strict, trap on invalid
    /// UTF-8) or their `_lossy` counterparts (substitute U+FFFD). Everything
    /// else reuses the ordinary
    /// loop/branch/break/index lowering, so move-checking, drop elaboration,
    /// and codegen come for free. The whole thing is preview-gated in Sema at
    /// the `@__rue_iter_len` intrinsic, which every for-loop emits.
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
            let init = self.gen_expr(coll_expr);
            let name = self.interner.get_or_intern(format!("__rue_for_coll_{n}"));
            let (ds, dl) = self.rir.add_directives(&[]);
            let alloc = self.rir.add_inst(Inst {
                data: InstData::Alloc {
                    directives_start: ds,
                    directives_len: dl,
                    name: Some(name),
                    is_mut: false,
                    ty: None,
                    init,
                    iter_elem: false,
                },
                span,
            });
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
        let (ds, dl) = self.rir.add_directives(&[]);
        let p_alloc = self.rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start: ds,
                directives_len: dl,
                name: Some(p_name),
                is_mut: true,
                ty: Some(u64_sym),
                init: zero,
                iter_elem: false,
            },
            span,
        });
        outer_stmts.push(p_alloc.as_u32());

        // let __len: u64 = @__rue_iter_len(__coll);
        // These two nodes carry the ITERABLE's span, not the whole statement's:
        // Sema's not-iterable type error (E0206) anchors on the intrinsic, and
        // it should underline the offending iterable expression.
        let iter_span = coll_expr.span();
        let len_name = self.interner.get_or_intern(format!("__rue_for_len_{n}"));
        let coll_for_len = self.rir.add_inst(Inst {
            data: InstData::VarRef { name: coll_name },
            span: iter_span,
        });
        let iter_len_sym = self.interner.get_or_intern("__rue_iter_len");
        let (la_start, la_len) = self.rir.add_inst_refs(&[coll_for_len]);
        let len_call = self.rir.add_inst(Inst {
            data: InstData::Intrinsic {
                name: iter_len_sym,
                args_start: la_start,
                args_len: la_len,
            },
            span: iter_span,
        });
        let (ds, dl) = self.rir.add_directives(&[]);
        let len_alloc = self.rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start: ds,
                directives_len: dl,
                name: Some(len_name),
                is_mut: false,
                ty: Some(u64_sym),
                init: len_call,
                iter_elem: false,
            },
            span,
        });
        outer_stmts.push(len_alloc.as_u32());

        // ---- loop body ----
        let mut body_stmts: Vec<u32> = Vec::new();

        // if __p >= __len { break }
        let p_ref1 = self.rir.add_inst(Inst {
            data: InstData::VarRef { name: p_name },
            span,
        });
        let len_ref = self.rir.add_inst(Inst {
            data: InstData::VarRef { name: len_name },
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
            data: InstData::VarRef { name: p_name },
            span,
        });
        let coll_for_get = self.rir.add_inst(Inst {
            data: InstData::VarRef { name: coll_name },
            span,
        });
        let get_inst = if is_chars {
            let sym = self.interner.get_or_intern(if is_lossy {
                "__rue_char_scalar_lossy"
            } else {
                "__rue_char_scalar"
            });
            let (s, l) = self.rir.add_inst_refs(&[coll_for_get, p_for_get]);
            self.rir.add_inst(Inst {
                data: InstData::Intrinsic {
                    name: sym,
                    args_start: s,
                    args_len: l,
                },
                span,
            })
        } else {
            self.rir.add_inst(Inst {
                data: InstData::IndexGet {
                    base: coll_for_get,
                    index: p_for_get,
                },
                span,
            })
        };
        let (ds, dl) = self.rir.add_directives(&[]);
        let binder_alloc = self.rir.add_inst(Inst {
            data: InstData::Alloc {
                directives_start: ds,
                directives_len: dl,
                name: binder_name,
                is_mut: false,
                ty: None,
                init: get_inst,
                // The element binding is a shared read of the collection
                // (spec 4.8:26): analyzed as a by-ref read so a non-Copy
                // element is not moved out (RUE-259), and a non-Copy binder is
                // a non-owning borrow slot the collection still drops.
                iter_elem: true,
            },
            span,
        });
        body_stmts.push(binder_alloc.as_u32());

        // __p = <advance>;   (advanced before the body so `continue` steps)
        let p_for_adv = self.rir.add_inst(Inst {
            data: InstData::VarRef { name: p_name },
            span,
        });
        let advance = if is_chars {
            let coll_for_adv = self.rir.add_inst(Inst {
                data: InstData::VarRef { name: coll_name },
                span,
            });
            let sym = self.interner.get_or_intern(if is_lossy {
                "__rue_char_next_lossy"
            } else {
                "__rue_char_next"
            });
            let (s, l) = self.rir.add_inst_refs(&[coll_for_adv, p_for_adv]);
            self.rir.add_inst(Inst {
                data: InstData::Intrinsic {
                    name: sym,
                    args_start: s,
                    args_len: l,
                },
                span,
            })
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
        let user_body = self.gen_block(&for_expr.body);
        body_stmts.push(user_body.as_u32());

        // block value = ()
        let body_unit = self.rir.add_inst(Inst {
            data: InstData::UnitConst,
            span,
        });
        body_stmts.push(body_unit.as_u32());

        let body_extra_start = self.rir.add_extra(&body_stmts);
        let loop_body = self.rir.add_inst(Inst {
            data: InstData::Block {
                extra_start: body_extra_start,
                len: body_stmts.len() as u32,
            },
            span,
        });

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

        let outer_extra_start = self.rir.add_extra(&outer_stmts);
        self.rir.add_inst(Inst {
            data: InstData::Block {
                extra_start: outer_extra_start,
                len: outer_stmts.len() as u32,
            },
            span,
        })
    }

    fn gen_statement(&mut self, stmt: &Statement) -> InstRef {
        match stmt {
            Statement::Let(let_stmt) => {
                let directives = self.convert_directives(&let_stmt.directives);
                let (directives_start, directives_len) = self.rir.add_directives(&directives);
                let name = match &let_stmt.pattern {
                    LetPattern::Ident(ident) => Some(self.symbol(ident.name)),
                    LetPattern::Wildcard(_) => None,
                };
                let ty = let_stmt.ty.as_ref().map(|t| self.intern_type(t));
                let init = self.gen_expr(&let_stmt.init);
                self.rir.add_inst(Inst {
                    data: InstData::Alloc {
                        directives_start,
                        directives_len,
                        name,
                        is_mut: let_stmt.is_mut,
                        ty,
                        init,
                        iter_elem: false,
                    },
                    span: let_stmt.span,
                })
            }
            Statement::Assign(assign) => {
                let value = self.gen_expr(&assign.value);
                match &assign.target {
                    AssignTarget::Var(ident) => self.rir.add_inst(Inst {
                        data: InstData::Assign {
                            name: self.symbol(ident.name),
                            value,
                        },
                        span: assign.span,
                    }),
                    AssignTarget::Field(field_expr) => {
                        let base = self.gen_expr(&field_expr.base);
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
                        let base = self.gen_expr(&index_expr.base);
                        let index = self.gen_expr(&index_expr.index);
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

        let astgen = AstGen::new(&ast, &interner);
        let rir = astgen.generate();
        (rir, interner)
    }

    #[test]
    fn borrowed_item_lowering_matches_reordered_ast_lowering() {
        let lexer = Lexer::new("fn first() -> i32 { 1 } fn main() -> i32 { first() }");
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();

        let reordered = Ast {
            items: ast.items.iter().rev().cloned().collect(),
        };
        let compatibility = AstGen::new(&reordered, &interner).generate();
        let borrowed = AstGen::generate_items(&interner, ast.items.iter().rev());

        assert_eq!(
            RirPrinter::new(&borrowed, &interner).to_string(),
            RirPrinter::new(&compatibility, &interner).to_string()
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
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                ..
            } => {
                assert_eq!(interner.resolve(&*name), "main");
                let params = rir.get_params(*params_start, *params_len);
                assert!(params.is_empty());
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
                    InstData::Block { extra_start, len } => {
                        // Block contains: Alloc, VarRef
                        assert_eq!(*len, 2);
                        let inst_refs = rir.get_extra(*extra_start, *len);
                        // Last instruction in block is the VarRef
                        let var_ref_inst = rir.get(InstRef::from_raw(inst_refs[1]));
                        match &var_ref_inst.data {
                            InstData::VarRef { name } => {
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
                    InstData::Block { extra_start, len } => {
                        // Block contains: Alloc(x), Alloc(y), Add
                        assert_eq!(*len, 3);
                        let inst_refs = rir.get_extra(*extra_start, *len);
                        // Last instruction in block is the Add
                        let add_inst = rir.get(InstRef::from_raw(inst_refs[2]));
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
            InstData::StructDecl {
                name,
                methods_start,
                methods_len,
                ..
            } => {
                assert_eq!(interner.resolve(&*name), "Point");
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
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
            InstData::StructDecl {
                methods_start,
                methods_len,
                ..
            } => {
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
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
                args_start,
                args_len,
            } => {
                assert_eq!(interner.resolve(&*method), "get_x");
                let args = rir.get_call_args(*args_start, *args_len);
                assert!(args.is_empty()); // No explicit args (self is implicit)
            }
            _ => panic!("expected MethodCall"),
        }
    }

    #[test]
    fn test_gen_assoc_fn_call() {
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
                args_start,
                args_len,
            } => {
                match &rir.get(*receiver).data {
                    InstData::VarRef { name } => assert_eq!(interner.resolve(name), "Point"),
                    other => panic!("expected VarRef receiver, got {other:?}"),
                }
                assert_eq!(interner.resolve(&*method), "origin");
                let args = rir.get_call_args(*args_start, *args_len);
                assert!(args.is_empty());
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
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
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
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
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
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
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
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
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
            InstData::Match {
                arms_start,
                arms_len,
                ..
            } => {
                let arms = rir.get_match_arms(*arms_start, *arms_len);
                assert_eq!(arms.len(), 3);

                // Check first arm is Color.Red
                match &arms[0].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(&*type_name), "Color");
                        assert_eq!(interner.resolve(&*variant), "Red");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check second arm is Color.Green
                match &arms[1].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(&*type_name), "Color");
                        assert_eq!(interner.resolve(&*variant), "Green");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check third arm is Color.Blue
                match &arms[2].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(&*type_name), "Color");
                        assert_eq!(interner.resolve(&*variant), "Blue");
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
            InstData::VarRef { name } => interner.resolve(&*name) == "self",
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
                    InstData::VarRef { name } => assert_eq!(interner.resolve(name), "Color"),
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
            InstData::StructDecl {
                methods_start,
                methods_len,
                ..
            } => {
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
                let method_inst = rir.get(methods[0]);
                match &method_inst.data {
                    InstData::FnDecl {
                        name,
                        params_start,
                        params_len,
                        has_self,
                        ..
                    } => {
                        assert_eq!(interner.resolve(&*name), "add");
                        assert!(*has_self);
                        // params should contain 'amount', not 'self'
                        let params = rir.get_params(*params_start, *params_len);
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
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => {
                // Should have 2 fields (x and y)
                let fields = rir.get_field_decls(*fields_start, *fields_len);
                assert_eq!(fields.len(), 2);
                assert_eq!(interner.resolve(&fields[0].0), "x");
                assert_eq!(interner.resolve(&fields[1].0), "y");

                // Should have 2 methods (get_x and origin)
                assert_eq!(*methods_len, 2);
                let methods = rir.get_inst_refs(*methods_start, *methods_len);
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
            InstData::AnonStructType { methods_len, .. } => {
                assert_eq!(*methods_len, 0, "Expected no methods");
            }
            _ => panic!("Expected AnonStructType"),
        }
    }
}
