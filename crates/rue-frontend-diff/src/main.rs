//! Paired token and structural differential for the production frontend and
//! `examples/ruelex`. Every source must first match the production token dump
//! byte-for-byte; only then is its lexeme-erased AST shape accepted.

use lasso::ThreadedRodeo;
use rue_lexer::Lexer;
use rue_parser::Parser;
use rue_parser::ast::*;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const BORROW: u64 = 10;
const INOUT: u64 = 12;
const MUT: u64 = 15;
const COMPTIME: u64 = 25;
const IDENT_TOKEN: u64 = 2;
const UNDERSCORE: u64 = 95;
const EXPECTED_CORPUS_FILES: usize = 117;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
const SYNTAX_PROBES: &[(&str, &str)] = &[
    (
        "intrinsic-placeholders.rue",
        "fn f(x: bool) { @probe(!, !x, (), [u8]); }",
    ),
    (
        "slice-types.rue",
        "fn f(borrow bytes: [u8]) -> u64 { @size_of([u8]) }",
    ),
];

fn node(kind: &str, meta: &str, a: String, b: String, c: String, d: String) -> String {
    format!("({kind}{meta} {a} {b} {c} {d})")
}

fn leaf(kind: &str) -> String {
    node(kind, "", "_".into(), "_".into(), "_".into(), "_".into())
}

fn list<I: IntoIterator<Item = String>>(items: I) -> String {
    items.into_iter().fold("_".to_owned(), |head, item| {
        node("list", "", head, item, "_".into(), "_".into())
    })
}

struct Shapes<'a> {
    interner: &'a ThreadedRodeo,
}

impl Shapes<'_> {
    fn ident(&self) -> String {
        leaf("ident")
    }

    fn directives(&self, directives: &[Directive]) -> String {
        list(directives.iter().map(|d| {
            node(
                "directive",
                "",
                list(d.args.iter().map(|arg| match arg {
                    DirectiveArg::Ident(_) => self.ident(),
                })),
                "_".into(),
                "_".into(),
                "_".into(),
            )
        }))
    }

    fn param_mode(mode: ParamMode) -> u64 {
        match mode {
            ParamMode::Normal => 0,
            ParamMode::Inout => INOUT,
            ParamMode::Borrow => BORROW,
            ParamMode::Comptime => COMPTIME,
        }
    }

    fn arg_mode(mode: ArgMode) -> u64 {
        match mode {
            ArgMode::Normal => 0,
            ArgMode::Inout => INOUT,
            ArgMode::Borrow => BORROW,
        }
    }

    fn param(&self, param: &Param) -> String {
        node(
            "param",
            &format!(" mode={}", Self::param_mode(param.mode)),
            self.ty(&param.ty),
            "_".into(),
            "_".into(),
            "_".into(),
        )
    }

    fn receiver(&self, receiver: &SelfParam) -> String {
        let mode = if receiver.is_mut {
            MUT
        } else {
            Self::param_mode(receiver.mode)
        };
        node(
            "param",
            &format!(" mode={mode}"),
            "_".into(),
            "_".into(),
            "_".into(),
            "_".into(),
        )
    }

    fn args(&self, args: &[CallArg]) -> String {
        list(args.iter().map(|arg| {
            node(
                "arg",
                &format!(" mode={}", Self::arg_mode(arg.mode)),
                self.expr(&arg.expr),
                "_".into(),
                "_".into(),
                "_".into(),
            )
        }))
    }

    fn array_length(&self, length: &ArrayLength) -> String {
        match length {
            ArrayLength::Literal(_) => leaf("int"),
            ArrayLength::Named(_) => self.ident(),
            ArrayLength::Call { args, .. } => node(
                "call",
                "",
                self.ident(),
                list(args.iter().map(|a| {
                    node(
                        "arg",
                        " mode=0",
                        self.array_length(a),
                        "_".into(),
                        "_".into(),
                        "_".into(),
                    )
                })),
                "_".into(),
                "_".into(),
            ),
        }
    }

    fn qualified(&self, count: usize) -> String {
        (1..count).fold(self.ident(), |base, _| {
            node("field", "", base, "_".into(), "_".into(), "_".into())
        })
    }

    fn ty(&self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Named(name) => {
                let spelling = self.interner.resolve(&name.name);
                let child = if matches!(
                    spelling,
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "bool"
                        | "type"
                        | "Self"
                ) {
                    "_".into()
                } else {
                    self.ident()
                };
                node(
                    "type",
                    " form=named",
                    child,
                    "_".into(),
                    "_".into(),
                    "_".into(),
                )
            }
            TypeExpr::Qualified { segments, .. } => node(
                "type",
                " form=named",
                self.qualified(segments.len()),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::Unit(_) => node(
                "type",
                " form=unit",
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::Never(_) => node(
                "type",
                " form=never",
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::Array {
                element, length, ..
            } => node(
                "array-type",
                "",
                self.ty(element),
                self.array_length(length),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::Slice { element, .. } => node(
                "slice-type",
                "",
                self.ty(element),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::AnonymousStruct {
                fields, methods, ..
            } => {
                let members = fields
                    .iter()
                    .map(|field| {
                        node(
                            "field-def",
                            "",
                            self.ty(&field.ty),
                            "_".into(),
                            "_".into(),
                            "_".into(),
                        )
                    })
                    .chain(methods.iter().map(|method| self.method(method)));
                node(
                    "anon-struct-type",
                    "",
                    list(members),
                    "_".into(),
                    "_".into(),
                    "_".into(),
                )
            }
            TypeExpr::AnonymousEnum { variants, .. } => node(
                "anon-enum-type",
                "",
                self.variants(variants),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::PointerConst { pointee, .. } => node(
                "ptr-type",
                " mode=36",
                self.ty(pointee),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::PointerMut { pointee, .. } => node(
                "ptr-type",
                " mode=15",
                self.ty(pointee),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::TypeCall { args, .. } | TypeExpr::QualifiedTypeCall { args, .. } => {
                let path = match ty {
                    TypeExpr::TypeCall { .. } => self.ident(),
                    TypeExpr::QualifiedTypeCall { segments, .. } => self.qualified(segments.len()),
                    _ => unreachable!(),
                };
                node(
                    "type-call",
                    "",
                    path,
                    list(args.iter().map(|a| self.ty(a))),
                    "_".into(),
                    "_".into(),
                )
            }
            TypeExpr::StrFixed { .. } => node(
                "type-call",
                "",
                self.ident(),
                list([leaf("int")]),
                "_".into(),
                "_".into(),
            ),
            TypeExpr::IntArg { .. } => leaf("int"),
        }
    }

    fn variants(&self, variants: &[EnumVariant]) -> String {
        list(variants.iter().map(|variant| {
            node(
                "variant",
                "",
                list(variant.payload.iter().map(|ty| self.ty(ty))),
                "_".into(),
                "_".into(),
                "_".into(),
            )
        }))
    }

    fn block(&self, block: &BlockExpr) -> String {
        let tail = match block.expr.as_ref() {
            Expr::Unit(unit) if unit.span.end == block.span.end => "_".into(),
            other => self.expr(other),
        };
        node(
            "block",
            "",
            list(block.statements.iter().map(|s| self.statement(s))),
            tail,
            "_".into(),
            "_".into(),
        )
    }

    fn bin_op(op: BinaryOp) -> u64 {
        match op {
            BinaryOp::Add => 60,
            BinaryOp::Sub => 61,
            BinaryOp::Mul => 62,
            BinaryOp::Div => 63,
            BinaryOp::Mod => 64,
            BinaryOp::Eq => 87,
            BinaryOp::Ne => 88,
            BinaryOp::Lt => 67,
            BinaryOp::Gt => 68,
            BinaryOp::Le => 89,
            BinaryOp::Ge => 90,
            BinaryOp::And => 93,
            BinaryOp::Or => 94,
            BinaryOp::BitAnd => 69,
            BinaryOp::BitOr => 70,
            BinaryOp::BitXor => 71,
            BinaryOp::Shl => 91,
            BinaryOp::Shr => 92,
        }
    }

    fn unary_op(op: UnaryOp) -> u64 {
        match op {
            UnaryOp::Neg => 61,
            UnaryOp::Not => 66,
            UnaryOp::BitNot => 72,
        }
    }

    fn expr(&self, expr: &Expr) -> String {
        match expr {
            Expr::Int(_) => leaf("int"),
            Expr::String(_) => leaf("string"),
            Expr::Bool(v) => node(
                "bool",
                &format!(" value={}", u8::from(v.value)),
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::Unit(_) => leaf("unit"),
            Expr::Ident(name) => {
                if self.interner.resolve(&name.name) == "Self" {
                    node(
                        "type",
                        " form=named",
                        "_".into(),
                        "_".into(),
                        "_".into(),
                        "_".into(),
                    )
                } else {
                    self.ident()
                }
            }
            Expr::SelfExpr(_) => self.ident(),
            Expr::Binary(v) => node(
                "binary",
                &format!(" op={}", Self::bin_op(v.op)),
                self.expr(&v.left),
                self.expr(&v.right),
                "_".into(),
                "_".into(),
            ),
            Expr::Unary(v) => node(
                "unary",
                &format!(" op={}", Self::unary_op(v.op)),
                self.expr(&v.operand),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::Paren(v) => self.expr(&v.inner),
            Expr::Block(v) => self.block(v),
            Expr::If(v) => {
                // The production AST represents `else if` as a synthetic
                // zero-statement block whose tail is the nested if. ruelex
                // keeps the grammar edge directly; erase only that wrapper.
                let else_shape = v.else_block.as_ref().map_or("_".into(), |b| {
                    if b.statements.is_empty()
                        && matches!(b.expr.as_ref(), Expr::If(_))
                        && b.span == b.expr.span()
                    {
                        self.expr(&b.expr)
                    } else {
                        self.block(b)
                    }
                });
                node(
                    "if",
                    "",
                    self.expr(&v.cond),
                    self.block(&v.then_block),
                    else_shape,
                    "_".into(),
                )
            }
            Expr::Match(v) => node(
                "match",
                "",
                self.expr(&v.scrutinee),
                list(v.arms.iter().map(|a| {
                    node(
                        "arm",
                        "",
                        self.pattern(&a.pattern),
                        self.expr(&a.body),
                        "_".into(),
                        "_".into(),
                    )
                })),
                "_".into(),
                "_".into(),
            ),
            Expr::While(v) => node(
                "while",
                "",
                self.expr(&v.cond),
                self.block(&v.body),
                "_".into(),
                "_".into(),
            ),
            Expr::Loop(v) => node(
                "loop",
                "",
                self.block(&v.body),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::For(v) => node(
                "for",
                &format!(
                    " binder={}",
                    match v.binder {
                        LetPattern::Ident(_) => IDENT_TOKEN,
                        LetPattern::Wildcard(_) => UNDERSCORE,
                    }
                ),
                self.expr(&v.iterable),
                self.block(&v.body),
                "_".into(),
                "_".into(),
            ),
            Expr::Call(v) => node(
                "call",
                "",
                self.ident(),
                self.args(&v.args),
                "_".into(),
                "_".into(),
            ),
            Expr::Break(v) => node(
                "break",
                "",
                v.value.as_ref().map_or("_".into(), |e| self.expr(e)),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::Continue(_) => leaf("continue"),
            Expr::Return(v) => node(
                "return",
                "",
                v.value.as_ref().map_or("_".into(), |e| self.expr(e)),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::StructLit(v) => {
                let mut head = v.base.as_ref().map_or_else(
                    || {
                        if self.interner.resolve(&v.name.name) == "Self" {
                            node(
                                "type",
                                " form=named",
                                "_".into(),
                                "_".into(),
                                "_".into(),
                                "_".into(),
                            )
                        } else {
                            self.ident()
                        }
                    },
                    |base| {
                        node(
                            "field",
                            "",
                            self.expr(base),
                            "_".into(),
                            "_".into(),
                            "_".into(),
                        )
                    },
                );
                if let Some(args) = &v.ctor_args {
                    head = node("call", "", head, self.args(args), "_".into(), "_".into());
                }
                node(
                    "struct-literal",
                    "",
                    head,
                    list(v.fields.iter().map(|f| {
                        node(
                            "field-init",
                            &format!(" shorthand={}", u8::from(f.shorthand)),
                            self.expr(&f.value),
                            "_".into(),
                            "_".into(),
                            "_".into(),
                        )
                    })),
                    "_".into(),
                    "_".into(),
                )
            }
            Expr::Field(v) => node(
                "field",
                "",
                self.expr(&v.base),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::MethodCall(v) => node(
                "method-call",
                "",
                self.expr(&v.receiver),
                self.args(&v.args),
                "_".into(),
                "_".into(),
            ),
            Expr::Try(v) => node(
                "try",
                "",
                self.expr(&v.operand),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::IntrinsicCall(v) => node(
                "intrinsic",
                "",
                list(v.args.iter().map(|a| {
                    let value = match a {
                        IntrinsicArg::Expr(e) => self.expr(e),
                        IntrinsicArg::Type(t) => self.ty(t),
                    };
                    node("arg", " mode=0", value, "_".into(), "_".into(), "_".into())
                })),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::ArrayLit(v) => {
                if let Some(repeat) = &v.repeat {
                    node(
                        "repeat-array",
                        "",
                        self.expr(&v.elements[0]),
                        self.array_length(repeat),
                        "_".into(),
                        "_".into(),
                    )
                } else {
                    node(
                        "array",
                        "",
                        list(v.elements.iter().map(|e| self.expr(e))),
                        "_".into(),
                        "_".into(),
                        "_".into(),
                    )
                }
            }
            Expr::Index(v) => node(
                "index",
                "",
                self.expr(&v.base),
                self.expr(&v.index),
                "_".into(),
                "_".into(),
            ),
            Expr::Path(v) => {
                let base = v.base.as_ref().map_or_else(
                    || self.ident(),
                    |e| {
                        node(
                            "field",
                            "",
                            self.expr(e),
                            "_".into(),
                            "_".into(),
                            "_".into(),
                        )
                    },
                );
                node("field", "", base, "_".into(), "_".into(), "_".into())
            }
            Expr::Comptime(v) => node(
                "comptime",
                "",
                self.expr(&v.expr),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::Checked(v) => node(
                "checked",
                "",
                self.expr(&v.expr),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Expr::TypeLit(v) => self.ty(&v.type_expr),
            Expr::Error(_) => leaf("error"),
        }
    }

    fn pattern(&self, pattern: &Pattern) -> String {
        match pattern {
            Pattern::Wildcard(_) => node(
                "pattern",
                " form=wildcard",
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Pattern::Int(_) => node(
                "pattern",
                " form=int",
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Pattern::NegInt(_) => node(
                "pattern",
                " form=neg-int",
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Pattern::Bool(v) => node(
                "pattern",
                if v.value { " form=true" } else { " form=false" },
                "_".into(),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Pattern::Path(v) => {
                let mut path = v.base.as_ref().map_or_else(
                    || self.ident(),
                    |e| {
                        node(
                            "field",
                            "",
                            self.expr(e),
                            "_".into(),
                            "_".into(),
                            "_".into(),
                        )
                    },
                );
                if let Some(args) = &v.ctor_args {
                    path = node(
                        "type-call",
                        "",
                        path,
                        list(args.iter().map(|a| {
                            // Qualified generic-pattern heads use CallArg in the
                            // production AST even though named arguments occupy
                            // type grammar positions. ruelex retains that type
                            // wrapper; reconstruct it from the interned spelling.
                            match &a.expr {
                                Expr::Ident(_) => node(
                                    "type",
                                    " form=named",
                                    self.ident(),
                                    "_".into(),
                                    "_".into(),
                                    "_".into(),
                                ),
                                Expr::Unit(_) => node(
                                    "type",
                                    " form=unit",
                                    "_".into(),
                                    "_".into(),
                                    "_".into(),
                                    "_".into(),
                                ),
                                other => self.expr(other),
                            }
                        })),
                        "_".into(),
                        "_".into(),
                    );
                }
                path = node("field", "", path, "_".into(), "_".into(), "_".into());
                node(
                    "pattern",
                    " form=path",
                    path,
                    list(v.bindings.iter().map(|_| self.ident())),
                    "_".into(),
                    "_".into(),
                )
            }
        }
    }

    fn statement(&self, statement: &Statement) -> String {
        match statement {
            Statement::Let(v) => node(
                "let",
                &format!(
                    " flags={}",
                    u64::from(v.is_mut)
                        + match v.pattern {
                            LetPattern::Ident(_) => IDENT_TOKEN * 256,
                            LetPattern::Wildcard(_) => UNDERSCORE * 256,
                        }
                ),
                self.directives(&v.directives),
                v.ty.as_ref().map_or("_".into(), |t| self.ty(t)),
                self.expr(&v.init),
                "_".into(),
            ),
            Statement::Assign(v) => node(
                "assign",
                "",
                match &v.target {
                    AssignTarget::Var(_) => self.ident(),
                    AssignTarget::Field(e) => self.expr(&Expr::Field(e.clone())),
                    AssignTarget::Index(e) => self.expr(&Expr::Index(e.clone())),
                },
                self.expr(&v.value),
                "_".into(),
                "_".into(),
            ),
            Statement::Expr(v) => node(
                "expr-stmt",
                "",
                self.expr(v),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
        }
    }

    fn method(&self, method: &Method) -> String {
        if self.interner.resolve(&method.name.name) == "__drop" {
            return node(
                "drop-fn",
                "",
                self.expr(&method.body),
                "_".into(),
                "_".into(),
                "_".into(),
            );
        }
        let params = method
            .receiver
            .iter()
            .map(|r| self.receiver(r))
            .chain(method.params.iter().map(|p| self.param(p)));
        node(
            "method",
            "",
            self.directives(&method.directives),
            list(params),
            method
                .return_type
                .as_ref()
                .map_or("_".into(), |t| self.ty(t)),
            self.expr(&method.body),
        )
    }

    fn function(&self, function: &Function) -> String {
        let flags = u64::from(function.visibility == Visibility::Public)
            + 2 * u64::from(function.is_unchecked);
        node(
            "function",
            &format!(" flags={flags}"),
            self.directives(&function.directives),
            list(function.params.iter().map(|p| self.param(p))),
            function
                .return_type
                .as_ref()
                .map_or("_".into(), |t| self.ty(t)),
            self.expr(&function.body),
        )
    }

    fn item(&self, item: &Item) -> String {
        match item {
            Item::Function(v) => self.function(v),
            Item::Struct(v) => {
                let flags =
                    u64::from(v.visibility == Visibility::Public) + 4 * u64::from(v.is_linear);
                let members = v
                    .fields
                    .iter()
                    .map(|f| {
                        (
                            f.span.start,
                            node(
                                "field-def",
                                "",
                                self.ty(&f.ty),
                                "_".into(),
                                "_".into(),
                                "_".into(),
                            ),
                        )
                    })
                    .chain(v.methods.iter().map(|m| (m.span.start, self.method(m))));
                let mut members: Vec<_> = members.collect();
                members.sort_by_key(|m| m.0);
                node(
                    "struct",
                    &format!(" flags={flags}"),
                    self.directives(&v.directives),
                    list(members.into_iter().map(|m| m.1)),
                    "_".into(),
                    "_".into(),
                )
            }
            Item::Enum(v) => node(
                "enum",
                &format!(" flags={}", u8::from(v.visibility == Visibility::Public)),
                self.variants(&v.variants),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Item::DropFn(v) => node(
                "drop-fn",
                "",
                self.expr(&v.body),
                "_".into(),
                "_".into(),
                "_".into(),
            ),
            Item::Const(v) => node(
                "const",
                &format!(" flags={}", u8::from(v.visibility == Visibility::Public)),
                self.directives(&v.directives),
                v.ty.as_ref().map_or("_".into(), |t| self.ty(t)),
                self.expr(&v.init),
                "_".into(),
            ),
            Item::Error(_) => leaf("error"),
        }
    }

    fn ast(&self, ast: &Ast) -> String {
        format!(
            "{}\n",
            node(
                "program",
                "",
                list(ast.items.iter().map(|i| self.item(i))),
                "_".into(),
                "_".into(),
                "_".into()
            )
        )
    }
}

struct ReferenceOutputs {
    tokens: String,
    shape: String,
}

fn rust_outputs(source: &str) -> Result<ReferenceOutputs, String> {
    let (tokens, interner) = Lexer::new(source)
        .tokenize()
        .map_err(|e| format!("lexer rejected source: {e:?}"))?;
    let token_dump = tokens.iter().map(|token| format!("{token}\n")).collect();
    let (ast, interner) = Parser::new(tokens, interner)
        .parse()
        .map_err(|e| format!("parser rejected source: {e:?}"))?;
    Ok(ReferenceOutputs {
        tokens: token_dump,
        shape: Shapes {
            interner: &interner,
        }
        .ast(&ast),
    })
}

#[cfg(test)]
fn rust_shape(source: &str) -> Result<String, String> {
    Ok(rust_outputs(source)?.shape)
}

fn collect_rue_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|e| format!("read {}: {e}", root.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_rue_files(&path, files)?;
        } else if path.extension().is_some_and(|e| e == "rue") {
            files.push(path);
        }
    }
    Ok(())
}

fn mismatch(kind: &str, path: &Path, expected: &str, actual: &str) -> String {
    let at = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    let start = at.saturating_sub(100);
    let end_e = (at + 180).min(expected.len());
    let end_a = (at + 180).min(actual.len());
    format!(
        "{}: first {kind} mismatch at byte {at}\nproduction: ...{}...\nruelex:    ...{}...",
        path.display(),
        &expected[start..end_e],
        &actual[start..end_a]
    )
}

fn bounded_run(command: Command, label: &str) -> Result<std::process::Output, String> {
    rue_test_runner::run_with_timeout_and_output_limit(
        command,
        PROCESS_TIMEOUT,
        None,
        MAX_CAPTURE_BYTES,
    )
    .map_err(|error| format!("{label}: {error}"))
}

fn frontend_output(frontend: &Path, path: &Path, shape: bool) -> Result<String, String> {
    let mut command = Command::new(frontend);
    if shape {
        command.arg("--ast-shape");
    }
    command.arg(path);
    let phase = if shape { "AST shape" } else { "token dump" };
    let output = bounded_run(command, &format!("ruelex {phase} for {}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "ruelex {phase} rejected {} ({}):\nstdout:\n{}\nstderr:\n{}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| {
        format!(
            "ruelex {phase} for {} was not UTF-8: {error}",
            path.display()
        )
    })
}

fn compare_source(frontend: &Path, path: &Path, source: &str) -> Result<(), String> {
    let production = rust_outputs(source)?;
    let ruelex_tokens = frontend_output(frontend, path, false)?;
    if production.tokens != ruelex_tokens {
        return Err(mismatch(
            "token/lexeme",
            path,
            &production.tokens,
            &ruelex_tokens,
        ));
    }
    let ruelex_shape = frontend_output(frontend, path, true)?;
    if production.shape != ruelex_shape {
        return Err(mismatch(
            "AST shape",
            path,
            &production.shape,
            &ruelex_shape,
        ));
    }
    Ok(())
}

fn run() -> Result<usize, String> {
    let compiler = env::var_os("RUE_BINARY").ok_or("RUE_BINARY is not set")?;
    let corpus =
        PathBuf::from(env::var_os("RUE_FRONTEND_DIFF_CORPUS").unwrap_or_else(|| ".".into()));
    let root = corpus.join("examples/ruelex/main.rue");
    if !root.is_file() {
        return Err(format!(
            "corpus root {} does not contain examples/ruelex/main.rue",
            corpus.display()
        ));
    }
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let frontend = temp.path().join("ruelex");
    let mut compile = Command::new(compiler);
    compile.arg(&root).arg("-o").arg(&frontend);
    let compile = bounded_run(compile, "compile ruelex")?;
    if !compile.status.success() {
        return Err(format!(
            "compiling {} failed with {}:\nstdout:\n{}\nstderr:\n{}",
            root.display(),
            compile.status,
            String::from_utf8_lossy(&compile.stdout),
            String::from_utf8_lossy(&compile.stderr)
        ));
    }
    let mut files = Vec::new();
    collect_rue_files(&corpus, &mut files)?;
    files.sort();
    if files.len() != EXPECTED_CORPUS_FILES {
        return Err(format!(
            "declared corpus contains {} .rue files, expected {EXPECTED_CORPUS_FILES}; update the explicit corpus and audited count together",
            files.len()
        ));
    }
    for path in &files {
        let source =
            fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        compare_source(&frontend, path, &source)?;
    }
    for (name, source) in SYNTAX_PROBES {
        let path = temp.path().join(name);
        fs::write(&path, source).map_err(|error| format!("write {}: {error}", path.display()))?;
        compare_source(&frontend, &path, source)?;
    }
    Ok(files.len())
}

fn main() -> ExitCode {
    match run() {
        Ok(count) => {
            println!(
                "frontend token + structural differential: {count}/{count} corpus files and {} syntax probes agree",
                SYNTAX_PROBES.len()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("frontend structural differential failed: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_frontend(tokens: &str, shape: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        fn shell_quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\"'\"'"))
        }

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("probe.rue");
        fs::write(&source, "fn main() {}").unwrap();
        let executable = temp.path().join("fake-ruelex");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --ast-shape ]; then printf %s {}; else printf %s {}; fi\n",
                shell_quote(shape),
                shell_quote(tokens)
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        (temp, executable, source)
    }

    #[test]
    fn normalization_covers_items_params_types_statements_control_flow_and_postfix() {
        let source = "pub unchecked fn f(comptime T: type, inout x: i32, borrow y: [u8; 4], borrow s: [u8]) -> i32 { let mut z: i32 = x; while z < 3 { z = z + 1; } if true { z } else { y[0] } }";
        let shape = rust_shape(source).unwrap();
        for needle in [
            "(function flags=3",
            "(param mode=25",
            "(array-type",
            "(slice-type",
            "(let flags=513",
            "(while",
            "(if",
            "(index",
            "(binary op=67",
        ] {
            assert!(shape.contains(needle), "missing {needle} in {shape}");
        }
    }

    #[test]
    fn normalization_covers_items_and_patterns() {
        let source = "pub struct S { x: i32, fn get(borrow self) -> i32 { self.x } } pub enum E { A, B(i32) } fn f(e: E) -> i32 { match e { E.A => 0, E.B(x) => x } }";
        let shape = rust_shape(source).unwrap();
        for needle in [
            "(struct flags=1",
            "(method ",
            "(enum flags=1",
            "(variant",
            "form=path",
            "(field",
        ] {
            assert!(shape.contains(needle), "missing {needle} in {shape}");
        }
    }

    #[test]
    fn mismatch_reports_first_difference_with_context() {
        let report = mismatch(
            "AST shape",
            Path::new("x.rue"),
            "(program (int))",
            "(program (bool))",
        );
        assert!(report.contains("x.rue: first AST shape mismatch at byte 10"));
        assert!(report.contains("production:"));
        assert!(report.contains("ruelex:"));
    }

    #[cfg(unix)]
    #[test]
    fn combined_oracle_rejects_mutated_token_output_even_when_shape_matches() {
        let source_text = "fn main() {}";
        let reference = rust_outputs(source_text).unwrap();
        let (_temp, frontend, source) = fake_frontend("mutated token dump\n", &reference.shape);
        let error = compare_source(&frontend, &source, source_text).unwrap_err();
        assert!(error.contains("token/lexeme mismatch"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn combined_oracle_rejects_mutated_shape_after_exact_tokens() {
        let source_text = "fn main() {}";
        let reference = rust_outputs(source_text).unwrap();
        let (_temp, frontend, source) = fake_frontend(&reference.tokens, "(mutated)\n");
        let error = compare_source(&frontend, &source, source_text).unwrap_err();
        assert!(error.contains("AST shape mismatch"), "{error}");
    }
}
