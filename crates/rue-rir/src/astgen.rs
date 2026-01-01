//! AST to RIR generation.
//!
//! AstGen converts the Abstract Syntax Tree into RIR instructions.
//! This is analogous to Zig's AstGen phase.

use lasso::{Spur, ThreadedRodeo};

/// Known type intrinsics that take a type argument rather than an expression.
/// These intrinsics operate on types at compile time (e.g., @size_of(i32)).
const TYPE_INTRINSICS: &[&str] = &["size_of", "align_of"];
use rue_parser::ast::DropFn;
use rue_parser::{
    ArgMode, AssignTarget, Ast, BinaryOp, CallArg, Directive, DirectiveArg, EnumDecl, Expr,
    Function, ImplBlock, IntrinsicArg, Item, LetPattern, Method, ParamMode, Pattern, Statement,
    StructDecl, TypeExpr, UnaryOp, ast::Visibility,
};

use crate::inst::{
    FunctionSpan, Inst, InstData, InstRef, Rir, RirArgMode, RirCallArg, RirDirective, RirParam,
    RirParamMode, RirPattern,
};

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// The AST being processed
    ast: &'a Ast,
    /// String interner for symbols (thread-safe, takes shared reference)
    interner: &'a ThreadedRodeo,
    /// Output RIR
    rir: Rir,
}

impl<'a> AstGen<'a> {
    /// Create a new AstGen for the given AST.
    pub fn new(ast: &'a Ast, interner: &'a ThreadedRodeo) -> Self {
        Self {
            ast,
            interner,
            rir: Rir::new(),
        }
    }

    /// Generate RIR from the AST.
    pub fn generate(mut self) -> Rir {
        for item in &self.ast.items {
            self.gen_item(item);
        }
        self.rir
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
            Item::Impl(impl_block) => {
                // Impl blocks are handled in Phase 2 (RIR Generation)
                // For now, store them for later processing by sema
                self.gen_impl_block(impl_block);
            }
            Item::DropFn(drop_fn) => {
                self.gen_drop_fn(drop_fn);
            }
        }
    }

    /// Convert a TypeExpr to its symbol representation.
    /// For named types, returns the existing symbol. For compound types, interns a new string.
    fn intern_type(&mut self, ty: &TypeExpr) -> Spur {
        match ty {
            TypeExpr::Named(ident) => ident.name, // Already a Spur
            TypeExpr::Unit(_) => self.interner.get_or_intern("()"),
            TypeExpr::Never(_) => self.interner.get_or_intern("!"),
            TypeExpr::Array {
                element, length, ..
            } => {
                // For arrays, we need to construct a string representation
                // Get the element symbol first, then look it up
                let elem_sym = self.intern_type(element);
                let elem_name = self.interner.resolve(&elem_sym);
                let s = format!("[{}; {}]", elem_name, length);
                self.interner.get_or_intern(&s)
            }
            TypeExpr::AnonymousStruct { fields, .. } => {
                // For anonymous structs, generate a canonical name representation
                let mut s = String::from("struct { ");
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let name = self.interner.resolve(&field.name.name);
                    let ty_sym = self.intern_type(&field.ty);
                    let ty_name = self.interner.resolve(&ty_sym);
                    s.push_str(name);
                    s.push_str(": ");
                    s.push_str(ty_name);
                }
                s.push_str(" }");
                self.interner.get_or_intern(&s)
            }
        }
    }

    fn gen_struct(&mut self, struct_decl: &StructDecl) -> InstRef {
        let directives = self.convert_directives(&struct_decl.directives);
        let (directives_start, directives_len) = self.rir.add_directives(&directives);
        let name = struct_decl.name.name; // Already a Spur
        let fields: Vec<_> = struct_decl
            .fields
            .iter()
            .map(|f| {
                let field_name = f.name.name; // Already a Spur
                let field_type = self.intern_type(&f.ty);
                (field_name, field_type)
            })
            .collect();
        let (fields_start, fields_len) = self.rir.add_field_decls(&fields);

        self.rir.add_inst(Inst {
            data: InstData::StructDecl {
                directives_start,
                directives_len,
                is_pub: struct_decl.visibility == Visibility::Public,
                is_linear: struct_decl.is_linear,
                name,
                fields_start,
                fields_len,
            },
            span: struct_decl.span,
        })
    }

    fn gen_enum(&mut self, enum_decl: &EnumDecl) -> InstRef {
        let name = enum_decl.name.name; // Already a Spur
        let variants: Vec<_> = enum_decl
            .variants
            .iter()
            .map(|v| v.name.name) // Already a Spur
            .collect();
        let (variants_start, variants_len) = self.rir.add_symbols(&variants);

        self.rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: enum_decl.visibility == Visibility::Public,
                name,
                variants_start,
                variants_len,
            },
            span: enum_decl.span,
        })
    }

    fn gen_impl_block(&mut self, impl_block: &ImplBlock) -> InstRef {
        let type_name = impl_block.type_name.name; // Already a Spur

        // Generate each method in the impl block
        let methods: Vec<_> = impl_block
            .methods
            .iter()
            .map(|m| self.gen_method(m))
            .collect();
        let (methods_start, methods_len) = self.rir.add_inst_refs(&methods);

        self.rir.add_inst(Inst {
            data: InstData::ImplDecl {
                type_name,
                methods_start,
                methods_len,
            },
            span: impl_block.span,
        })
    }

    fn gen_drop_fn(&mut self, drop_fn: &DropFn) -> InstRef {
        let type_name = drop_fn.type_name.name; // Already a Spur

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
        let name = method.name.name; // Already a Spur
        let return_type = match &method.return_type {
            Some(ty) => self.intern_type(ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters (excluding self, which is handled specially by sema)
        let params: Vec<_> = method
            .params
            .iter()
            .map(|p| RirParam {
                name: p.name.name, // Already a Spur
                ty: self.intern_type(&p.ty),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.is_comptime,
            })
            .collect();
        let (params_start, params_len) = self.rir.add_params(&params);

        // Record the body start before generating body instructions
        let body_start = InstRef::from_raw(self.rir.current_inst_index());

        // Generate body expression
        let body = self.gen_expr(&method.body);

        // Track whether this method has a self receiver (method vs associated function)
        let has_self = method.receiver.is_some();

        // Emit methods as FnDecl instructions with has_self flag.
        // Sema uses has_self to add the implicit self parameter for methods.
        // Methods don't have their own visibility - they're accessible if the type is accessible.
        let decl = self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: false,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
            },
            span: method.span,
        });

        // Record the function span for per-function analysis
        // Methods are also tracked as functions for per-function analysis
        self.rir
            .add_function_span(FunctionSpan::new(name, body_start, decl));

        decl
    }

    /// Convert AST directives to RIR directives
    fn convert_directives(&mut self, directives: &[Directive]) -> Vec<RirDirective> {
        directives
            .iter()
            .map(|d| RirDirective {
                name: d.name.name, // Already a Spur
                args: d
                    .args
                    .iter()
                    .map(|arg| match arg {
                        DirectiveArg::Ident(ident) => ident.name, // Already a Spur
                    })
                    .collect(),
                span: d.span,
            })
            .collect()
    }

    /// Convert AST ParamMode to RIR RirParamMode
    fn convert_param_mode(&self, mode: ParamMode) -> RirParamMode {
        match mode {
            ParamMode::Normal => RirParamMode::Normal,
            ParamMode::Inout => RirParamMode::Inout,
            ParamMode::Borrow => RirParamMode::Borrow,
            ParamMode::Comptime => RirParamMode::Comptime,
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
        let name = func.name.name; // Already a Spur
        let return_type = match &func.return_type {
            Some(ty) => self.intern_type(ty),
            None => self.interner.get_or_intern("()"), // Default to unit type
        };

        // Convert parameters
        let params: Vec<_> = func
            .params
            .iter()
            .map(|p| RirParam {
                name: p.name.name, // Already a Spur
                ty: self.intern_type(&p.ty),
                mode: self.convert_param_mode(p.mode),
                is_comptime: p.is_comptime,
            })
            .collect();
        let (params_start, params_len) = self.rir.add_params(&params);

        // Record the body start before generating body instructions
        let body_start = InstRef::from_raw(self.rir.current_inst_index());

        // Generate body expression
        let body = self.gen_expr(&func.body);

        // Create function declaration instruction
        // Regular functions don't have a self receiver
        let decl = self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                directives_start,
                directives_len,
                is_pub: func.visibility == Visibility::Public,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: false,
            },
            span: func.span,
        });

        // Record the function span for per-function analysis
        self.rir
            .add_function_span(FunctionSpan::new(name, body_start, decl));

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
            Expr::String(lit) => {
                self.rir.add_inst(Inst {
                    data: InstData::StringConst(lit.value), // Already a Spur
                    span: lit.span,
                })
            }
            Expr::Unit(lit) => self.rir.add_inst(Inst {
                data: InstData::UnitConst,
                span: lit.span,
            }),
            Expr::Ident(ident) => {
                self.rir.add_inst(Inst {
                    data: InstData::VarRef { name: ident.name }, // Already a Spur
                    span: ident.span,
                })
            }
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
            Expr::Loop(loop_expr) => {
                let body = self.gen_block(&loop_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::InfiniteLoop { body },
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
                        name: call.name.name, // Already a Spur
                        args_start,
                        args_len,
                    },
                    span: call.span,
                })
            }
            Expr::Break(break_expr) => self.rir.add_inst(Inst {
                data: InstData::Break,
                span: break_expr.span,
            }),
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
                let fields: Vec<_> = struct_lit
                    .fields
                    .iter()
                    .map(|f| {
                        let field_value = self.gen_expr(&f.value);
                        (f.name.name, field_value) // name is already a Symbol
                    })
                    .collect();
                let (fields_start, fields_len) = self.rir.add_field_inits(&fields);

                self.rir.add_inst(Inst {
                    data: InstData::StructInit {
                        type_name: struct_lit.name.name, // Already a Spur
                        fields_start,
                        fields_len,
                    },
                    span: struct_lit.span,
                })
            }
            Expr::Field(field_expr) => {
                let base = self.gen_expr(&field_expr.base);

                self.rir.add_inst(Inst {
                    data: InstData::FieldGet {
                        base,
                        field: field_expr.field.name, // Already a Spur
                    },
                    span: field_expr.span,
                })
            }
            Expr::IntrinsicCall(intrinsic) => {
                let name = intrinsic.name.name; // Already a Spur
                let intrinsic_name_str = self.interner.resolve(&name);

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
                                type_arg: ident.name, // Already a Spur
                            },
                            span: intrinsic.span,
                        });
                    }
                }

                // Otherwise, treat as an expression intrinsic
                let args: Vec<_> = intrinsic
                    .args
                    .iter()
                    .filter_map(|a| match a {
                        IntrinsicArg::Expr(expr) => Some(self.gen_expr(expr)),
                        IntrinsicArg::Type(_) => None, // This shouldn't happen for expr intrinsics
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
            Expr::Index(index_expr) => {
                let base = self.gen_expr(&index_expr.base);
                let index = self.gen_expr(&index_expr.index);

                self.rir.add_inst(Inst {
                    data: InstData::IndexGet { base, index },
                    span: index_expr.span,
                })
            }
            Expr::Path(path_expr) => {
                self.rir.add_inst(Inst {
                    data: InstData::EnumVariant {
                        type_name: path_expr.type_name.name, // Already a Spur
                        variant: path_expr.variant.name,     // Already a Spur
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
                        method: method_call.method.name, // Already a Spur
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
                        type_name: assoc_fn_call.type_name.name, // Already a Spur
                        function: assoc_fn_call.function.name,   // Already a Spur
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
            Expr::TypeLit(type_lit) => {
                // Generate a type constant instruction for type-as-value expressions
                match &type_lit.type_expr {
                    TypeExpr::AnonymousStruct { fields, .. } => {
                        // Generate an anonymous struct type instruction
                        let field_decls: Vec<(Spur, Spur)> = fields
                            .iter()
                            .map(|f| {
                                let name = f.name.name;
                                let ty = self.intern_type(&f.ty);
                                (name, ty)
                            })
                            .collect();
                        let (fields_start, fields_len) = self.rir.add_field_decls(&field_decls);
                        self.rir.add_inst(Inst {
                            data: InstData::AnonStructType {
                                fields_start,
                                fields_len,
                            },
                            span: type_lit.span,
                        })
                    }
                    _ => {
                        // For named types, unit, never, and arrays, generate TypeConst
                        let type_name = match &type_lit.type_expr {
                            TypeExpr::Named(ident) => ident.name,
                            TypeExpr::Unit(_) => self.interner.get_or_intern_static("()"),
                            TypeExpr::Never(_) => self.interner.get_or_intern_static("!"),
                            TypeExpr::Array { .. } => {
                                // Array types as values are not yet supported
                                // For now, use a placeholder
                                self.interner.get_or_intern_static("array")
                            }
                            TypeExpr::AnonymousStruct { .. } => {
                                unreachable!("handled above")
                            }
                        };
                        self.rir.add_inst(Inst {
                            data: InstData::TypeConst { type_name },
                            span: type_lit.span,
                        })
                    }
                }
            }
        }
    }

    fn gen_pattern(&mut self, pattern: &Pattern) -> RirPattern {
        match pattern {
            Pattern::Wildcard(span) => RirPattern::Wildcard(*span),
            Pattern::Int(lit) => RirPattern::Int(lit.value as i64, lit.span),
            // Use wrapping_neg to handle i64::MIN correctly (where value is 9223372036854775808)
            Pattern::NegInt(lit) => RirPattern::Int((lit.value as i64).wrapping_neg(), lit.span),
            Pattern::Bool(lit) => RirPattern::Bool(lit.value, lit.span),
            Pattern::Path(path) => {
                RirPattern::Path {
                    type_name: path.type_name.name, // Already a Spur
                    variant: path.variant.name,     // Already a Spur
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

    fn gen_statement(&mut self, stmt: &Statement) -> InstRef {
        match stmt {
            Statement::Let(let_stmt) => {
                let directives = self.convert_directives(&let_stmt.directives);
                let (directives_start, directives_len) = self.rir.add_directives(&directives);
                let name = match &let_stmt.pattern {
                    LetPattern::Ident(ident) => Some(ident.name), // Already a Spur
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
                    },
                    span: let_stmt.span,
                })
            }
            Statement::Assign(assign) => {
                let value = self.gen_expr(&assign.value);
                match &assign.target {
                    AssignTarget::Var(ident) => {
                        self.rir.add_inst(Inst {
                            data: InstData::Assign {
                                name: ident.name, // Already a Spur
                                value,
                            },
                            span: assign.span,
                        })
                    }
                    AssignTarget::Field(field_expr) => {
                        let base = self.gen_expr(&field_expr.base);
                        self.rir.add_inst(Inst {
                            data: InstData::FieldSet {
                                base,
                                field: field_expr.field.name, // Already a Spur
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

    // Impl block tests
    #[test]
    fn test_gen_impl_block() {
        let source = r#"
            struct Point { x: i32, y: i32 }
            impl Point {
                fn get_x(self) -> i32 {
                    self.x
                }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the ImplDecl instruction
        let impl_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::ImplDecl { .. }));
        assert!(impl_decl.is_some(), "Expected ImplDecl instruction");

        let (_, inst) = impl_decl.unwrap();
        match &inst.data {
            InstData::ImplDecl {
                type_name,
                methods_start,
                methods_len,
            } => {
                assert_eq!(interner.resolve(&*type_name), "Point");
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
            _ => panic!("expected ImplDecl"),
        }
    }

    #[test]
    fn test_gen_impl_block_with_multiple_methods() {
        let source = r#"
            struct Point { x: i32, y: i32 }
            impl Point {
                fn get_x(self) -> i32 { self.x }
                fn get_y(self) -> i32 { self.y }
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        let impl_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::ImplDecl { .. }));
        assert!(impl_decl.is_some());

        let (_, inst) = impl_decl.unwrap();
        match &inst.data {
            InstData::ImplDecl {
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
            _ => panic!("expected ImplDecl"),
        }
    }

    #[test]
    fn test_gen_method_call() {
        let source = r#"
            struct Point { x: i32 }
            impl Point {
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
        let source = r#"
            struct Point { x: i32, y: i32 }
            impl Point {
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 {
                let p = Point::origin();
                0
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the AssocFnCall instruction
        let assoc_fn_call = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::AssocFnCall { .. }));
        assert!(assoc_fn_call.is_some(), "Expected AssocFnCall instruction");

        let (_, inst) = assoc_fn_call.unwrap();
        match &inst.data {
            InstData::AssocFnCall {
                type_name,
                function,
                args_start,
                args_len,
            } => {
                assert_eq!(interner.resolve(&*type_name), "Point");
                assert_eq!(interner.resolve(&*function), "origin");
                let args = rir.get_call_args(*args_start, *args_len);
                assert!(args.is_empty());
            }
            _ => panic!("expected AssocFnCall"),
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
                assert!(matches!(arms[0].0, RirPattern::Int(1, _)));
                assert!(matches!(arms[1].0, RirPattern::Int(2, _)));
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
                assert!(matches!(arms[0].0, RirPattern::Int(-5, _)));
                assert!(matches!(arms[1].0, RirPattern::Int(-10, _)));
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
                let c = Color::Red;
                match c {
                    Color::Red => 1,
                    Color::Green => 2,
                    Color::Blue => 3,
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

                // Check first arm is Color::Red
                match &arms[0].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(&*type_name), "Color");
                        assert_eq!(interner.resolve(&*variant), "Red");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check second arm is Color::Green
                match &arms[1].0 {
                    RirPattern::Path {
                        type_name, variant, ..
                    } => {
                        assert_eq!(interner.resolve(&*type_name), "Color");
                        assert_eq!(interner.resolve(&*variant), "Green");
                    }
                    _ => panic!("expected Path pattern"),
                }

                // Check third arm is Color::Blue
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
            struct Point { x: i32 }
            impl Point {
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
        let source = r#"
            enum Color { Red, Green, Blue }
            fn main() -> i32 {
                let c = Color::Red;
                0
            }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the EnumVariant instruction
        let enum_variant = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::EnumVariant { .. }));
        assert!(enum_variant.is_some(), "Expected EnumVariant instruction");

        let (_, inst) = enum_variant.unwrap();
        match &inst.data {
            InstData::EnumVariant { type_name, variant } => {
                assert_eq!(interner.resolve(&*type_name), "Color");
                assert_eq!(interner.resolve(&*variant), "Red");
            }
            _ => panic!("expected EnumVariant"),
        }
    }

    #[test]
    fn test_gen_method_with_params() {
        let source = r#"
            struct Counter { value: i32 }
            impl Counter {
                fn add(self, amount: i32) -> i32 { self.value + amount }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the method FnDecl
        let impl_decl = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::ImplDecl { .. }));
        assert!(impl_decl.is_some());

        let (_, inst) = impl_decl.unwrap();
        match &inst.data {
            InstData::ImplDecl {
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
            _ => panic!("expected ImplDecl"),
        }
    }

    // RirPrinter integration test with actual generated RIR
    #[test]
    fn test_printer_integration() {
        let source = r#"
            struct Point { x: i32, y: i32 }
            impl Point {
                fn origin() -> Point { Point { x: 0, y: 0 } }
            }
            fn main() -> i32 {
                let p = Point::origin();
                p.x
            }
        "#;
        let (rir, interner) = gen_rir(source);

        let printer = RirPrinter::new(&rir, &interner);
        let output = printer.to_string();

        // Check key elements are present in the output
        assert!(output.contains("struct Point"));
        assert!(output.contains("impl Point"));
        assert!(output.contains("fn origin"));
        assert!(output.contains("fn main"));
        assert!(output.contains("struct_init Point"));
        assert!(output.contains("assoc_fn_call Point::origin"));
        assert!(output.contains("field_get"));
    }

    // ===== Function span tests =====

    #[test]
    fn test_function_spans_simple() {
        let (rir, interner) = gen_rir("fn main() -> i32 { 42 }");

        // Should have exactly one function span
        assert_eq!(rir.function_count(), 1);

        let spans: Vec<_> = rir.functions().collect();
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(interner.resolve(&span.name), "main");

        // The function should have 2 instructions: IntConst(42) and FnDecl
        assert_eq!(span.instruction_count(), 2);

        // The FnDecl should be the last instruction
        let fn_inst = rir.get(span.decl);
        assert!(matches!(fn_inst.data, InstData::FnDecl { .. }));
    }

    #[test]
    fn test_function_spans_multiple_functions() {
        let source = r#"
            fn helper() -> i32 { 1 }
            fn main() -> i32 { 42 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Should have two function spans
        assert_eq!(rir.function_count(), 2);

        let spans: Vec<_> = rir.functions().collect();
        assert_eq!(spans.len(), 2);

        // First function: helper
        assert_eq!(interner.resolve(&spans[0].name), "helper");
        assert_eq!(spans[0].instruction_count(), 2);

        // Second function: main
        assert_eq!(interner.resolve(&spans[1].name), "main");
        assert_eq!(spans[1].instruction_count(), 2);

        // Function spans should be non-overlapping
        assert!(
            spans[0].decl.as_u32() < spans[1].body_start.as_u32(),
            "helper should end before main starts"
        );
    }

    #[test]
    fn test_function_spans_with_methods() {
        let source = r#"
            struct Point { x: i32 }
            impl Point {
                fn get_x(self) -> i32 { self.x }
                fn origin() -> Point { Point { x: 0 } }
            }
            fn main() -> i32 { 0 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Should have three function spans: get_x, origin, main
        assert_eq!(rir.function_count(), 3);

        let spans: Vec<_> = rir.functions().collect();

        // Methods should be tracked as well
        let names: Vec<_> = spans.iter().map(|s| interner.resolve(&s.name)).collect();
        assert!(names.contains(&"get_x"));
        assert!(names.contains(&"origin"));
        assert!(names.contains(&"main"));
    }

    #[test]
    fn test_function_view() {
        let source = r#"
            fn helper(x: i32) -> i32 { x + 1 }
            fn main() -> i32 { helper(41) }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find the main function span
        let main_span = rir.find_function(interner.get_or_intern("main")).unwrap();

        // Get a view of main's instructions
        let view = rir.function_view(main_span);

        // The view should contain the right number of instructions
        assert_eq!(view.len(), main_span.instruction_count() as usize);

        // The last instruction should be the FnDecl
        let fn_decl = view.fn_decl();
        match &fn_decl.data {
            InstData::FnDecl { name, .. } => {
                assert_eq!(interner.resolve(&*name), "main");
            }
            _ => panic!("Expected FnDecl"),
        }

        // We should be able to iterate over the view
        let mut found_call = false;
        for (_, inst) in view.iter() {
            if matches!(inst.data, InstData::Call { .. }) {
                found_call = true;
            }
        }
        assert!(found_call, "main should contain a call to helper");
    }

    #[test]
    fn test_function_span_complex_body() {
        let source = r#"
            fn complex() -> i32 {
                let x = 1;
                let y = 2;
                if x < y {
                    x + y
                } else {
                    x - y
                }
            }
        "#;
        let (rir, interner) = gen_rir(source);

        assert_eq!(rir.function_count(), 1);

        let span = rir
            .find_function(interner.get_or_intern("complex"))
            .unwrap();

        // The function should have multiple instructions for the body
        // At minimum: 2 IntConsts, 2 Allocs, comparison, branches, operations, FnDecl
        assert!(
            span.instruction_count() >= 8,
            "Complex function should have at least 8 instructions, got {}",
            span.instruction_count()
        );

        // Verify the view contains all expected instruction types
        let view = rir.function_view(span);
        let mut has_alloc = false;
        let mut has_branch = false;

        for (_, inst) in view.iter() {
            if matches!(inst.data, InstData::Alloc { .. }) {
                has_alloc = true;
            }
            if matches!(inst.data, InstData::Branch { .. }) {
                has_branch = true;
            }
        }

        assert!(has_alloc, "Function should have Alloc instructions");
        assert!(has_branch, "Function should have Branch instruction");
    }

    #[test]
    fn test_find_function() {
        let source = r#"
            fn foo() -> i32 { 1 }
            fn bar() -> i32 { 2 }
            fn baz() -> i32 { 3 }
        "#;
        let (rir, interner) = gen_rir(source);

        // Find existing functions
        let foo_sym = interner.get_or_intern("foo");
        let bar_sym = interner.get_or_intern("bar");
        let baz_sym = interner.get_or_intern("baz");
        let nonexistent_sym = interner.get_or_intern("nonexistent");

        assert!(rir.find_function(foo_sym).is_some());
        assert!(rir.find_function(bar_sym).is_some());
        assert!(rir.find_function(baz_sym).is_some());
        assert!(rir.find_function(nonexistent_sym).is_none());
    }

    #[test]
    fn test_function_span_ordering() {
        let source = r#"
            fn a() -> i32 { 1 }
            fn b() -> i32 { 2 }
            fn c() -> i32 { 3 }
        "#;
        let (rir, _interner) = gen_rir(source);

        let spans: Vec<_> = rir.functions().collect();
        assert_eq!(spans.len(), 3);

        // Verify functions are recorded in source order
        for i in 1..spans.len() {
            assert!(
                spans[i - 1].decl.as_u32() < spans[i].body_start.as_u32(),
                "Function {} should end before function {} starts",
                i - 1,
                i
            );
        }
    }
}
