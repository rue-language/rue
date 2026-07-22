//! Definition-relative anchors for value-position anonymous type literals.
//!
//! This walk mirrors [`crate::AstGen`]'s structural-path discipline exactly for
//! the subset of the expression grammar that can enclose a value-position
//! anonymous `struct`/`enum` type literal, and yields the same
//! [`RirStructuralAnchor`] `AstGen` mints for each such literal.
//!
//! It is the single anchor authority the durable declaration-time comptime
//! evaluator consumes. That evaluator reparses one isolated declaration fragment
//! whose `FileId(0)` spans are disjoint from module RIR, so the frontend anchor
//! cannot be reconstructed by looking a fragment span up in module RIR after the
//! fact. Instead the anchors are computed against the module AST — where the
//! anchor scheme is defined — and transported to the fragment out of band.
//!
//! A method body of an anonymous struct is deliberately NOT descended: `AstGen`
//! resets it to its own producer root (`with_producer_root`), and the durable
//! evaluator records only member signatures, never member bodies, when it
//! reduces an enclosing producer. Each such method body is a separate producer
//! whose own anchors are collected by walking it as a fresh root.

use rue_parser::ast::{AssignTarget, BlockExpr, Expr, IntrinsicArg, Pattern, Statement, TypeExpr};
use rue_span::Span;

use crate::{RirStructuralAnchor, RirStructuralPathSegment as Seg};

/// The two anonymous nominal kinds a literal can introduce. Kept RIR-local so
/// this crate does not depend on the semantic identity domain; the compiler
/// maps it onto `rue_air::AnonymousNominalKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnonymousTypeSiteKind {
    Struct,
    Enum,
}

/// One value-position anonymous type literal, with the exact `AstGen` anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonymousTypeSite {
    /// Span of the anonymous literal's `TypeExpr`, in the coordinate space of
    /// the source the walk ran against. Identity never; a transport locator.
    pub span: Span,
    pub kind: AnonymousTypeSiteKind,
    pub anchor: RirStructuralAnchor,
}

/// Collect every value-position anonymous type literal reachable while reducing
/// `root` as a producer body, each carrying the exact `AstGen` anchor.
///
/// `root` is the declaration's body expression — a constant initializer or a
/// function/method/destructor body — which `AstGen` lowers via
/// `gen_expr_at(Body, root)`. The walk therefore starts one `Body` segment deep,
/// matching `AstGen`.
pub fn anonymous_type_sites(root: &Expr) -> Vec<AnonymousTypeSite> {
    let mut walker = SiteWalker {
        path: vec![Seg::Body],
        sites: Vec::new(),
    };
    walker.walk_expr(root);
    walker.sites
}

struct SiteWalker {
    path: Vec<Seg>,
    sites: Vec<AnonymousTypeSite>,
}

impl SiteWalker {
    fn at(&mut self, segment: Seg, action: impl FnOnce(&mut Self)) {
        self.path.push(segment);
        action(self);
        self.path.pop();
    }

    fn record(&mut self, span: Span, kind: AnonymousTypeSiteKind) {
        let mut segments = self.path.clone();
        segments.push(Seg::AnonymousType(0));
        self.sites.push(AnonymousTypeSite {
            span,
            kind,
            anchor: RirStructuralAnchor::new(segments),
        });
    }

    /// Mirrors `gen_block`: the block body is one `Body` segment deep, then its
    /// statements are `Statement(i)` and its value is `Operand(0)`
    /// (`gen_block_contents`, for both the empty- and non-empty-statement forms).
    fn walk_block(&mut self, block: &BlockExpr) {
        self.at(Seg::Body, |this| {
            for (index, statement) in block.statements.iter().enumerate() {
                this.at(Seg::Statement(index as u32), |this| {
                    this.walk_statement(statement)
                });
            }
            this.at(Seg::Operand(0), |this| this.walk_expr(&block.expr));
        });
    }

    fn walk_statement(&mut self, statement: &Statement) {
        match statement {
            // `let` type annotations are annotation position (no anonymous
            // literal); the initializer is `Operand(0)`.
            Statement::Let(let_stmt) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&let_stmt.init));
            }
            Statement::Assign(assign) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&assign.value));
                match &assign.target {
                    AssignTarget::Var(_) => {}
                    AssignTarget::Field(field) => {
                        self.at(Seg::Operand(1), |this| this.walk_expr(&field.base));
                    }
                    AssignTarget::Index(index) => {
                        self.at(Seg::Operand(1), |this| this.walk_expr(&index.base));
                        self.at(Seg::Operand(2), |this| this.walk_expr(&index.index));
                    }
                }
            }
            // A bare expression statement is lowered by `gen_expr` with no extra
            // segment (the enclosing `Statement(i)` is pushed by `walk_block`).
            Statement::Expr(expr) => self.walk_expr(expr),
        }
    }

    fn walk_call_args(&mut self, args: &[rue_parser::ast::CallArg], base: u32) {
        for (index, arg) in args.iter().enumerate() {
            self.at(Seg::Operand(base + index as u32), |this| {
                this.walk_expr(&arg.expr)
            });
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int(_)
            | Expr::Bool(_)
            | Expr::String(_)
            | Expr::Unit(_)
            | Expr::Ident(_)
            | Expr::SelfExpr(_)
            | Expr::Continue(_)
            | Expr::Error(_) => {}
            Expr::Binary(bin) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&bin.left));
                self.at(Seg::Operand(1), |this| this.walk_expr(&bin.right));
            }
            Expr::Unary(un) => self.at(Seg::Operand(0), |this| this.walk_expr(&un.operand)),
            Expr::Try(try_expr) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&try_expr.operand))
            }
            // Parentheses are transparent in `gen_expr` (no segment).
            Expr::Paren(paren) => self.walk_expr(&paren.inner),
            Expr::Block(block) => self.walk_block(block),
            Expr::If(if_expr) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&if_expr.cond));
                self.at(Seg::Branch(0), |this| this.walk_block(&if_expr.then_block));
                if let Some(else_block) = &if_expr.else_block {
                    self.at(Seg::Branch(1), |this| this.walk_block(else_block));
                }
            }
            Expr::While(while_expr) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&while_expr.cond));
                self.at(Seg::Branch(0), |this| this.walk_block(&while_expr.body));
            }
            Expr::Loop(loop_expr) => {
                self.at(Seg::Branch(0), |this| this.walk_block(&loop_expr.body))
            }
            Expr::For(for_expr) => {
                // `gen_for` lowers a non-variable iterable at `Operand(0)` and the
                // user body at `Branch(0)`. A bare-variable iterable is referenced
                // by name (no operand). The `.chars()` view unwrap is not modelled
                // here: string iterables carry no anonymous literals, and any
                // residual divergence fails closed loudly rather than miscompiling.
                if !matches!(&*for_expr.iterable, Expr::Ident(_)) {
                    self.at(Seg::Operand(0), |this| this.walk_expr(&for_expr.iterable));
                }
                self.at(Seg::Branch(0), |this| this.walk_block(&for_expr.body));
            }
            Expr::Match(match_expr) => {
                self.at(Seg::Operand(0), |this| {
                    this.walk_expr(&match_expr.scrutinee)
                });
                for (index, arm) in match_expr.arms.iter().enumerate() {
                    self.at(Seg::MatchArm(index as u32), |this| {
                        this.at(Seg::Operand(0), |this| this.walk_pattern(&arm.pattern));
                        this.at(Seg::Operand(1), |this| this.walk_expr(&arm.body));
                    });
                }
            }
            Expr::Call(call) => self.walk_call_args(&call.args, 0),
            Expr::Break(break_expr) => {
                if let Some(value) = &break_expr.value {
                    self.at(Seg::Operand(0), |this| this.walk_expr(value));
                }
            }
            Expr::Return(return_expr) => {
                if let Some(value) = &return_expr.value {
                    self.at(Seg::Operand(0), |this| this.walk_expr(value));
                }
            }
            Expr::StructLit(struct_lit) => {
                if let Some(base) = &struct_lit.base {
                    self.at(Seg::Operand(0), |this| this.walk_expr(base));
                }
                if let Some(ctor_args) = &struct_lit.ctor_args {
                    self.walk_call_args(ctor_args, 1);
                }
                for (index, field) in struct_lit.fields.iter().enumerate() {
                    self.at(Seg::FieldType(index as u32), |this| {
                        this.walk_expr(&field.value)
                    });
                }
            }
            Expr::Field(field) => self.at(Seg::Operand(0), |this| this.walk_expr(&field.base)),
            Expr::Index(index_expr) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&index_expr.base));
                self.at(Seg::Operand(1), |this| this.walk_expr(&index_expr.index));
            }
            Expr::MethodCall(method_call) => {
                self.at(Seg::Operand(0), |this| {
                    this.walk_expr(&method_call.receiver)
                });
                self.walk_call_args(&method_call.args, 1);
            }
            Expr::Path(path) => {
                // A path expression carries only an optional module base; inline
                // type-constructor heads are a pattern-only form (`walk_pattern`).
                if let Some(base) = &path.base {
                    self.at(Seg::Operand(0), |this| this.walk_expr(base));
                }
            }
            Expr::IntrinsicCall(intrinsic) => {
                // `AstGen` enumerates every argument to keep operand indices
                // stable; a type argument becomes a `TypeConst` placeholder with
                // no anchor. The `@offset_of` / type-intrinsic early returns also
                // carry no anonymous literal (their type argument is interned, not
                // anchored), so enumerating and recursing only into expression
                // arguments reproduces the anchor set exactly.
                for (index, arg) in intrinsic.args.iter().enumerate() {
                    if let IntrinsicArg::Expr(inner) = arg {
                        self.at(Seg::Operand(index as u32), |this| this.walk_expr(inner));
                    }
                }
            }
            Expr::ArrayLit(array_lit) => {
                if array_lit.repeat.is_some() {
                    if let Some(value) = array_lit.elements.first() {
                        self.at(Seg::Operand(0), |this| this.walk_expr(value));
                    }
                } else {
                    for (index, element) in array_lit.elements.iter().enumerate() {
                        self.at(Seg::Operand(index as u32), |this| this.walk_expr(element));
                    }
                }
            }
            Expr::Comptime(comptime) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&comptime.expr))
            }
            Expr::Checked(checked) => {
                self.at(Seg::Operand(0), |this| this.walk_expr(&checked.expr))
            }
            Expr::TypeLit(type_lit) => match &type_lit.type_expr {
                TypeExpr::AnonymousStruct { .. } => {
                    // Record the struct, but do not descend: its field types are
                    // annotation position (forbidden anonymous literals) and its
                    // methods are separate producer roots.
                    self.record(type_lit.type_expr.span(), AnonymousTypeSiteKind::Struct);
                }
                TypeExpr::AnonymousEnum { .. } => {
                    self.record(type_lit.type_expr.span(), AnonymousTypeSiteKind::Enum);
                }
                _ => {}
            },
        }
    }

    fn walk_pattern(&mut self, pattern: &Pattern) {
        if let Pattern::Path(path) = pattern {
            if let Some(base) = &path.base {
                self.at(Seg::Operand(0), |this| this.walk_expr(base));
            }
            if let Some(ctor_args) = &path.ctor_args {
                self.walk_call_args(ctor_args, 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AstGen, InstData};
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_parser::ast::Item;

    /// Anchors `AstGen` mints, in dense RIR order.
    fn rir_anonymous_anchors(source: &str) -> Vec<RirStructuralAnchor> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, interner) = parser.parse().unwrap();
        let mut astgen = AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        rir.iter()
            .filter_map(|(_, instruction)| match &instruction.data {
                InstData::AnonStructType { anchor, .. } | InstData::AnonEnumType { anchor, .. } => {
                    Some(anchor.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Anchors the walk mints across every TOP-LEVEL producer root — the only
    /// roots the durable evaluator reduces. Test programs are chosen so no
    /// anonymous literal is nested inside an anonymous struct's method body (a
    /// separate producer root not walked here), making the walk's multiset the
    /// complete frontend set.
    fn walk_anonymous_anchors(source: &str) -> Vec<RirStructuralAnchor> {
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, _interner) = parser.parse().unwrap();
        let mut anchors = Vec::new();
        for item in &ast.items {
            let roots: Vec<&Expr> = match item {
                Item::Function(function) => vec![&function.body],
                Item::Const(constant) => vec![&constant.init],
                Item::DropFn(drop_fn) => vec![&drop_fn.body],
                _ => Vec::new(),
            };
            for root in roots {
                anchors.extend(
                    anonymous_type_sites(root)
                        .into_iter()
                        .map(|site| site.anchor),
                );
            }
        }
        anchors
    }

    fn assert_walk_matches_rir(source: &str) {
        let mut walk = walk_anonymous_anchors(source);
        let mut rir = rir_anonymous_anchors(source);
        walk.sort_by(|a, b| a.segments().cmp(b.segments()));
        rir.sort_by(|a, b| a.segments().cmp(b.segments()));
        assert_eq!(
            walk, rir,
            "walk anchors diverged from AstGen for:\n{source}"
        );
    }

    /// Frontend bijection at the mint, PER PRODUCER: within one declaration body
    /// every anonymous site maps to exactly one anchor and no two distinct sites
    /// share an anchor. (Distinct producers may reuse the same relative anchor;
    /// their identities differ by producer, so uniqueness is producer-local.)
    #[test]
    fn walk_anchors_are_unique_within_one_producer() {
        // A single producer with several same- and different-kind sites in
        // distinct positions.
        let source = r#"
const K = {
    let _a = struct { a: i32 };
    let _b = struct { b: i32 };
    let _c = enum { X, Y };
    enum { Z }
};
"#;
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, _interner) = parser.parse().unwrap();
        let Item::Const(constant) = &ast.items[0] else {
            panic!("expected a const");
        };
        let sites = anonymous_type_sites(&constant.init);
        assert_eq!(sites.len(), 4, "expected four sites in the producer");
        let anchors: std::collections::BTreeSet<_> = sites
            .iter()
            .map(|site| site.anchor.segments().to_vec())
            .collect();
        assert_eq!(
            anchors.len(),
            sites.len(),
            "two distinct sites in one producer share an anchor",
        );
    }

    #[test]
    fn walk_reproduces_astgen_anchors_for_type_producers() {
        // Generic struct producer whose method reaches its anonymous-enum field
        // (the RUE-1089 Wrap repro), plus its Option producer.
        assert_walk_matches_rir(
            r#"
fn Option(comptime T: type) -> type { enum { Some(T), None } }
fn Wrap(comptime T: type) -> type {
    struct {
        inner: Option(T),
        fn get_or(self, d: T) -> T {
            let O = Option(T);
            match self.inner { O.Some(v) => v, O.None => d }
        }
    }
}
"#,
        );
    }

    #[test]
    fn walk_reproduces_astgen_anchors_for_direct_const_literal() {
        assert_walk_matches_rir("const P = struct { x: i32, y: i32 };");
        assert_walk_matches_rir("const E = enum { A, B };");
    }

    #[test]
    fn walk_reproduces_astgen_anchors_for_comptime_branches() {
        // Both branches carry a same-kind anonymous literal; each must anchor to
        // its own branch position.
        assert_walk_matches_rir(
            r#"
fn Pick(comptime b: bool) -> type {
    if b { struct { x: i32 } } else { struct { y: i32 } }
}
"#,
        );
    }

    #[test]
    fn walk_reproduces_astgen_anchors_for_statement_bound_literals() {
        // A const whose block binds several anonymous literals in statements plus
        // a tail literal: each lands on its own Statement(i) / Operand(0) path.
        assert_walk_matches_rir(
            r#"
const K = {
    let _a = struct { a: i32 };
    let _b = enum { X, Y };
    struct { c: i32 }
};
"#,
        );
    }

    #[test]
    fn walk_reproduces_astgen_anchors_for_match_arms() {
        assert_walk_matches_rir(
            r#"
fn Choose(comptime n: i32) -> type {
    match n {
        0 => struct { z: i32 },
        _ => enum { P, Q },
    }
}
"#,
        );
    }

    #[test]
    fn walk_records_kind_per_site() {
        let source = "const E = enum { A, B };";
        let lexer = Lexer::new(source);
        let (tokens, interner) = lexer.tokenize().unwrap();
        let parser = Parser::new(tokens, interner);
        let (ast, _interner) = parser.parse().unwrap();
        let Item::Const(constant) = &ast.items[0] else {
            panic!("expected a const");
        };
        let sites = anonymous_type_sites(&constant.init);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].kind, AnonymousTypeSiteKind::Enum);
    }
}
