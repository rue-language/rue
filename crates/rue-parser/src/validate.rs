//! Post-parse AST validation.
//!
//! Checks that need the interner (resolving names to strings for the
//! diagnostic) and therefore can't easily run inside the chumsky
//! combinators, which only carry pre-interned symbols in their state.
//!
//! Currently this validates @-directive names: an unknown directive such as
//! `@important fn main() ...` used to be silently accepted and ignored,
//! turning typos like `@alllow(...)` into no-ops. Unknown directives are now
//! a compile error naming the directive. (RUE-133)
//!
//! Note this covers *directive* position only (before items and `let`
//! statements). Expression-position `@name(...)` intrinsics (`@dbg`,
//! `@syscall`, `@intCast`, ...) are validated by sema, which already rejects
//! unknown intrinsics.

use crate::ast::{Ast, Directive, Expr, IntrinsicArg, Item, Method, Statement, TypeExpr};
use lasso::ThreadedRodeo;
use rue_error::{CompileError, ErrorKind};

/// Directive names the compiler understands.
///
/// This is the single source of truth for *directive* (item/statement
/// position) names; keep it in sync with the consumers in sema:
/// - `allow`  — suppresses lints, e.g. `@allow(unused_variable)` on `let`
///   (see `has_allow_directive` in rue-air)
/// - `copy`   — marks a struct as a copy type (see `has_copy_directive`)
pub const KNOWN_DIRECTIVES: &[&str] = &["allow", "copy"];

/// Walk the AST and report every directive whose name is not in
/// [`KNOWN_DIRECTIVES`].
pub fn check_directives(ast: &Ast, interner: &ThreadedRodeo) -> Vec<CompileError> {
    let mut v = Validator {
        interner,
        errors: Vec::new(),
    };
    for item in &ast.items {
        v.check_item(item);
    }
    v.errors
}

struct Validator<'a> {
    interner: &'a ThreadedRodeo,
    errors: Vec<CompileError>,
}

impl Validator<'_> {
    fn check_directives(&mut self, directives: &[Directive]) {
        for directive in directives {
            let name = self.interner.resolve(&directive.name.name);
            if !KNOWN_DIRECTIVES.contains(&name) {
                self.errors.push(CompileError::new(
                    ErrorKind::ParseError(format!(
                        "unknown directive '@{}'; known directives are @allow and @copy",
                        name
                    )),
                    directive.span,
                ));
            }
        }
    }

    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Function(f) => {
                self.check_directives(&f.directives);
                self.check_expr(&f.body);
            }
            Item::Struct(s) => {
                self.check_directives(&s.directives);
                for method in &s.methods {
                    self.check_method(method);
                }
            }
            Item::Enum(_) => {}
            Item::DropFn(d) => self.check_expr(&d.body),
            Item::Const(c) => {
                self.check_directives(&c.directives);
                self.check_expr(&c.init);
            }
            Item::Error(_) => {}
        }
    }

    fn check_method(&mut self, method: &Method) {
        self.check_directives(&method.directives);
        self.check_expr(&method.body);
    }

    fn check_statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Let(l) => {
                self.check_directives(&l.directives);
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
            | Expr::String(_)
            | Expr::Bool(_)
            | Expr::Unit(_)
            | Expr::Ident(_)
            | Expr::Continue(_)
            | Expr::SelfExpr(_)
            | Expr::Error(_) => {}
            Expr::Binary(b) => {
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
            Expr::StructLit(s) => {
                if let Some(base) = &s.base {
                    self.check_expr(base);
                }
                for field in &s.fields {
                    self.check_expr(&field.value);
                }
            }
            Expr::Field(f) => self.check_expr(&f.base),
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
            Expr::AssocFnCall(a) => {
                if let Some(base) = &a.base {
                    self.check_expr(base);
                }
                for arg in &a.args {
                    self.check_expr(&arg.expr);
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
            TypeExpr::Named(_) | TypeExpr::Unit(_) | TypeExpr::Never(_) => {}
            TypeExpr::Array { element, .. } => self.check_type_expr(element),
            TypeExpr::AnonymousStruct { methods, .. } => {
                for method in methods {
                    self.check_method(method);
                }
            }
            TypeExpr::PointerConst { pointee, .. } | TypeExpr::PointerMut { pointee, .. } => {
                self.check_type_expr(pointee)
            }
        }
    }
}
