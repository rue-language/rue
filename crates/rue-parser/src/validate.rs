//! Post-parse AST validation.
//!
//! Checks that need the interner to resolve names for diagnostics and are
//! clearer as whole-AST policy than as token grammar decisions.
//!
//! Currently this validates @-directive names, arguments, and placement: an
//! unknown directive such as `@important fn main() ...` used to be silently
//! accepted and ignored, turning typos like `@alllow(...)` into no-ops.
//! Unknown directives are now a compile error naming the directive. (RUE-133)
//!
//! Note this covers *directive* position only (before items and `let`
//! statements). Expression-position `@name(...)` intrinsics (`@dbg`,
//! `@syscall`, `@intCast`, ...) are validated by sema, which already rejects
//! unknown intrinsics.

use crate::ast::{Ast, Directive, Expr, IntrinsicArg, Item, Method, Statement, TypeExpr};
use crate::parser_policy::diagnostics::ParserDiagnostics;
use lasso::ThreadedRodeo;
use rue_error::{CompileError, ErrorKind};

/// Directive names the compiler understands.
///
/// This is the single source of truth for *directive* (item/statement
/// position) names; keep it in sync with the consumers in sema:
/// - `allow`  — suppresses lints, e.g. `@allow(unused_variable)` on `let`
///   (see `has_allow_directive` in rue-air)
/// - `copy`   — marks a struct as a copy type (see `has_copy_directive`)
/// - `repr`   — the C representation guarantee marker `@repr(c)` on a struct
///   (ADR-0064 Amendment 1; see `has_repr_c_directive` in rue-air)
pub const KNOWN_DIRECTIVES: &[&str] = &["allow", "copy", "repr"];

/// Representation arguments accepted by `@repr(...)`.
///
/// Only `c` is accepted in v0 (ADR-0064 Amendment 1). The parameterized grammar
/// stays open for future `@repr(packed)` / `@repr(align(n))`; parsing validates
/// the argument so `@repr(packed)` fails loudly with the accepted set named,
/// rather than being silently ignored.
pub const KNOWN_REPR_ARGS: &[&str] = &["c"];

/// Warning names accepted by `@allow(...)`.
///
/// Parsing validates spelling so typos like `@allow(unused_variabl)` fail
/// loudly; sema consumes these names when emitting and suppressing warnings
/// (RUE-356).
pub const KNOWN_WARNING_NAMES: &[&str] =
    &["unused_variable", "unused_function", "unreachable_code"];

/// Walk the AST and report directive-validation diagnostics through the same
/// bounded per-file policy as grammar recovery.
pub fn check_directives(ast: &Ast, interner: &ThreadedRodeo) -> Vec<CompileError> {
    let mut v = Validator {
        interner,
        errors: ParserDiagnostics::default(),
    };
    for item in &ast.items {
        v.check_item(item);
    }
    v.errors.finish().0
}

struct Validator<'a> {
    interner: &'a ThreadedRodeo,
    errors: ParserDiagnostics,
}

#[derive(Clone, Copy)]
enum DirectiveSite {
    Function,
    Struct,
    Method,
    Const,
    Let,
}

impl DirectiveSite {
    fn description(self) -> &'static str {
        match self {
            DirectiveSite::Function => "functions",
            DirectiveSite::Struct => "structs",
            DirectiveSite::Method => "methods",
            DirectiveSite::Const => "const declarations",
            DirectiveSite::Let => "let statements",
        }
    }
}

impl Validator<'_> {
    fn check_directives(&mut self, directives: &[Directive], site: DirectiveSite) {
        for directive in directives {
            let name = self.interner.resolve(&directive.name.name);
            if !KNOWN_DIRECTIVES.contains(&name) {
                self.errors.push(CompileError::new(
                    ErrorKind::ParseError(format!(
                        "unknown directive '@{}'; known directives are @allow, @copy, and @repr",
                        name
                    )),
                    directive.span,
                ));
                continue;
            }

            match name {
                "copy" => {
                    if !matches!(site, DirectiveSite::Struct) {
                        self.errors.push(CompileError::new(
                            ErrorKind::ParseError(format!(
                                "@copy can only be applied to structs, not {}",
                                site.description()
                            )),
                            directive.span,
                        ));
                    } else if !directive.args.is_empty() {
                        // Arity is per-directive: @copy takes no arguments
                        // (spec 2.5), while @allow legitimately takes lint names.
                        self.errors.push(CompileError::new(
                            ErrorKind::ParseError("@copy takes no arguments".to_string()),
                            directive.span,
                        ));
                    }
                }
                "repr" => {
                    // `@repr(c)` is the C representation guarantee marker
                    // (ADR-0064 Amendment 1). It applies only to structs, and
                    // is parameterized: exactly one argument, `c` in v0. Unknown
                    // arguments (e.g. `@repr(packed)`) fail loudly here naming
                    // the accepted set, leaving the grammar open for future
                    // packed/align representations.
                    if !matches!(site, DirectiveSite::Struct) {
                        self.errors.push(CompileError::new(
                            ErrorKind::ParseError(format!(
                                "@repr can only be applied to structs, not {}",
                                site.description()
                            )),
                            directive.span,
                        ));
                    } else if directive.args.len() != 1 {
                        self.errors.push(CompileError::new(
                            ErrorKind::ParseError(format!(
                                "@repr takes exactly one representation argument; \
                                 accepted arguments are {}",
                                KNOWN_REPR_ARGS.join(", ")
                            )),
                            directive.span,
                        ));
                    } else {
                        let crate::ast::DirectiveArg::Ident(ident) = &directive.args[0];
                        let arg = self.interner.resolve(&ident.name);
                        if !KNOWN_REPR_ARGS.contains(&arg) {
                            self.errors.push(CompileError::new(
                                ErrorKind::ParseError(format!(
                                    "unknown @repr argument '{arg}'; accepted arguments are {}",
                                    KNOWN_REPR_ARGS.join(", ")
                                )),
                                ident.span,
                            ));
                        }
                    }
                }
                "allow" => {
                    if !matches!(
                        site,
                        DirectiveSite::Function | DirectiveSite::Method | DirectiveSite::Let
                    ) {
                        self.errors.push(CompileError::new(
                            ErrorKind::ParseError(format!(
                                "@allow can only be applied to functions, methods, or let statements, not {}",
                                site.description()
                            )),
                            directive.span,
                        ));
                    }

                    for arg in &directive.args {
                        let crate::ast::DirectiveArg::Ident(ident) = arg;
                        let warning = self.interner.resolve(&ident.name);
                        if !KNOWN_WARNING_NAMES.contains(&warning) {
                            self.errors.push(CompileError::new(
                                ErrorKind::ParseError(format!(
                                    "unrecognized warning name '{warning}' in @allow; known warnings are {}",
                                    KNOWN_WARNING_NAMES.join(", ")
                                )),
                                ident.span,
                            ));
                        }
                    }
                }
                _ => unreachable!("known directive list and validator are out of sync"),
            }
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => {
                self.check_directives(&f.directives, DirectiveSite::Function);
                self.check_expr(&f.body);
            }
            Item::Struct(s) => {
                self.check_directives(&s.directives, DirectiveSite::Struct);
                for method in &s.methods {
                    self.check_method(method);
                }
            }
            Item::Enum(_) => {}
            Item::DropFn(d) => self.check_expr(&d.body),
            Item::Extern(_) => {}
            Item::Const(c) => {
                self.check_directives(&c.directives, DirectiveSite::Const);
                self.check_expr(&c.init);
            }
            Item::Error(_) => {}
        }
    }

    fn check_method(&mut self, method: &Method) {
        self.check_directives(&method.directives, DirectiveSite::Method);
        self.check_expr(&method.body);
    }

    fn check_statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Let(l) => {
                self.check_directives(&l.directives, DirectiveSite::Let);
                self.check_expr(&l.init);
            }
            Statement::Assign(a) => self.check_expr(&a.value),
            Statement::Expr(e) => self.check_expr(e),
        }
    }

    /// Recurse into every sub-expression that can contain a block (and thus
    /// `let` statements carrying directives) or an anonymous struct type
    /// (whose methods carry directives).
    fn check_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Bool(_)
            | Expr::Unit(_)
            | Expr::Ident(_)
            | Expr::Continue(_)
            | Expr::SelfExpr(_)
            | Expr::Error(_) => {}
            Expr::Binary(b) => {
                // Chained comparison check (spec 4.3:12, RUE-528). Comparison
                // operators are left-associative and share one precedence
                // level, so `a < b == c` parses as `(a < b) == c` with the
                // inner comparison DIRECTLY as the left operand. This is the
                // only paren-aware place to check: RIR erases parentheses, so
                // the old sema check also rejected the explicitly
                // parenthesized `(a < b) == c` — an ordinary boolean
                // equality, not a chain. An explicit `Expr::Paren` between
                // the two comparisons breaks the chain (anything ill-typed
                // inside is then diagnosed normally by sema).
                if is_comparison_op(b.op)
                    && matches!(&*b.left, Expr::Binary(inner) if is_comparison_op(inner.op))
                {
                    self.errors.push(
                        CompileError::new(ErrorKind::ChainedComparison, b.span).with_help(
                            "use `&&` to combine comparisons (`a < b && b < c`), or \
                             parenthesize a boolean operand: `(a < b) == c`",
                        ),
                    );
                }
                self.check_expr(&b.left);
                self.check_expr(&b.right);
            }
            Expr::Unary(u) => self.check_expr(&u.operand),
            Expr::Paren(p) => self.check_expr(&p.inner),
            Expr::Block(b) => {
                for statement in &b.statements {
                    self.check_statement(statement);
                }
                self.check_expr(&b.expr);
            }
            Expr::If(i) => {
                self.check_expr(&i.cond);
                self.check_expr_block(&i.then_block);
                if let Some(else_block) = &i.else_block {
                    self.check_expr_block(else_block);
                }
            }
            Expr::Match(m) => {
                self.check_expr(&m.scrutinee);
                for arm in &m.arms {
                    self.check_expr(&arm.body);
                }
            }
            Expr::While(w) => {
                self.check_expr(&w.cond);
                self.check_expr_block(&w.body);
            }
            Expr::Loop(l) => self.check_expr_block(&l.body),
            Expr::For(fe) => {
                self.check_expr(&fe.iterable);
                self.check_expr_block(&fe.body);
            }
            Expr::Call(c) => {
                for arg in &c.args {
                    self.check_expr(&arg.expr);
                }
            }
            Expr::Break(b) => {
                if let Some(value) = &b.value {
                    self.check_expr(value);
                }
            }
            Expr::Return(r) => {
                if let Some(value) = &r.value {
                    self.check_expr(value);
                }
            }
            Expr::Yield(y) => self.check_expr(&y.value),
            Expr::StructLit(s) => {
                if let Some(base) = &s.base {
                    self.check_expr(base);
                }
                for field in &s.fields {
                    self.check_expr(&field.value);
                }
            }
            Expr::Field(f) => self.check_expr(&f.base),
            Expr::Try(t) => self.check_expr(&t.operand),
            Expr::MethodCall(m) => {
                self.check_expr(&m.receiver);
                for arg in &m.args {
                    self.check_expr(&arg.expr);
                }
            }
            Expr::IntrinsicCall(i) => {
                for arg in &i.args {
                    match arg {
                        IntrinsicArg::Expr(e) => self.check_expr(e),
                        IntrinsicArg::Type(ty) => self.check_type_expr(ty),
                    }
                }
            }
            Expr::ArrayLit(a) => {
                for element in &a.elements {
                    self.check_expr(element);
                }
            }
            Expr::Index(i) => {
                self.check_expr(&i.base);
                self.check_expr(&i.index);
            }
            Expr::Path(p) => {
                if let Some(base) = &p.base {
                    self.check_expr(base);
                }
            }
            Expr::Comptime(c) => self.check_expr(&c.expr),
            Expr::Checked(c) => self.check_expr(&c.expr),
            Expr::TypeLit(t) => self.check_type_expr(&t.type_expr),
        }
    }

    fn check_expr_block(&mut self, block: &crate::ast::BlockExpr) {
        for statement in &block.statements {
            self.check_statement(statement);
        }
        self.check_expr(&block.expr);
    }

    /// Anonymous struct types (comptime type construction) contain methods,
    /// which carry directives and bodies of their own.
    fn check_type_expr(&mut self, ty: &TypeExpr) {
        match ty {
            TypeExpr::Named(_)
            | TypeExpr::Qualified { .. }
            | TypeExpr::Unit(_)
            | TypeExpr::Never(_) => {}
            TypeExpr::Array { element, .. } => self.check_type_expr(element),
            TypeExpr::Slice { element, .. } => self.check_type_expr(element),
            TypeExpr::AnonymousStruct { methods, .. } => {
                for method in methods {
                    self.check_method(method);
                }
            }
            // Anonymous enum types carry only variant payload type expressions;
            // no nested methods/bodies to validate.
            TypeExpr::AnonymousEnum { .. } => {}
            TypeExpr::PointerConst { pointee, .. } | TypeExpr::PointerMut { pointee, .. } => {
                self.check_type_expr(pointee)
            }
            // Type-function application: recurse into each type argument.
            TypeExpr::TypeCall { args, .. } | TypeExpr::QualifiedTypeCall { args, .. } => {
                for arg in args {
                    self.check_type_expr(arg);
                }
            }
            // Fixed-capacity string `Str(N)`: a scalar literal capacity, no
            // nested type expressions to validate (ADR-0043 Phase 5, RUE-326).
            TypeExpr::StrFixed { .. } => {}
            // Integer type-call argument (RUE-552): a scalar literal, nothing
            // nested to validate.
            TypeExpr::IntArg { .. } => {}
        }
    }
}

/// Whether `op` is one of the six comparison operators, which share a single
/// non-chainable precedence level (spec 4.3:12).
fn is_comparison_op(op: crate::ast::BinaryOp) -> bool {
    use crate::ast::BinaryOp;
    matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge
    )
}
