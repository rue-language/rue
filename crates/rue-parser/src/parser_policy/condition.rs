//! Reclassification for brace groups greedily parsed as struct literals.
//!
//! A bare control-flow head cannot contain an unparenthesized struct literal,
//! so ambiguous empty and shorthand brace groups are reclaimed as the body.

use crate::ast::{BlockExpr, CallExpr, Expr, FieldExpr, MethodCallExpr, StructLitExpr, UnitLit};
use rue_span::Span;

fn empty_body(lit: &StructLitExpr) -> BlockExpr {
    let span = Span::with_file(lit.span.file_id, lit.name.span.end, lit.span.end);
    BlockExpr {
        statements: Vec::new(),
        expr: Box::new(Expr::Unit(UnitLit { span })),
        span,
    }
}

fn reclaim_head(lit: StructLitExpr) -> Option<Expr> {
    let StructLitExpr {
        base,
        name,
        ctor_args,
        span,
        ..
    } = lit;
    Some(match (base, ctor_args) {
        (None, Some(args)) => Expr::Call(CallExpr { name, args, span }),
        // A module-qualified inline type-constructor head with an ambiguous
        // empty/shorthand brace group (`m.f(args) {}`, RUE-951) is the method
        // call plus the control-flow body, mirroring how the local call head
        // `f(args) {}` reclaims to a bare `Call`. A meaningful struct literal
        // (explicit fields) does not reach here — its brace group is non-empty
        // and non-shorthand, so reclamation fails and the bare-condition
        // diagnostic fires instead.
        (Some(base), Some(args)) => Expr::MethodCall(MethodCallExpr {
            receiver: base,
            method: name,
            args,
            span,
        }),
        (None, None) => Expr::Ident(name),
        (Some(base), None) => {
            let span = base.span().extend_to(name.span.end);
            Expr::Field(FieldExpr {
                base,
                field: name,
                span,
            })
        }
    })
}

pub(crate) fn reclaim_empty_struct_lit_head(lit: StructLitExpr) -> Option<Expr> {
    lit.fields
        .is_empty()
        .then_some(())
        .and_then(|()| reclaim_head(lit))
}

/// Whether the right spine ends in a struct literal. This is used only to
/// select the precise diagnostic after reclamation fails.
pub(crate) fn tail_is_struct_lit(expr: &Expr) -> bool {
    match expr {
        Expr::StructLit(_) => true,
        Expr::Binary(binary) => tail_is_struct_lit(&binary.right),
        Expr::Unary(unary) => tail_is_struct_lit(&unary.operand),
        _ => false,
    }
}

/// Reclaim an ambiguous struct-literal tail as a control-flow condition and
/// body while preserving all original spans.
pub(crate) fn reclaim_as_condition_and_body(cond: Expr) -> Option<(Expr, BlockExpr)> {
    if let Expr::StructLit(lit) = &cond {
        if lit.fields.is_empty() {
            let body = empty_body(lit);
            let Expr::StructLit(lit) = cond else {
                unreachable!()
            };
            return Some((reclaim_head(lit)?, body));
        }
    }
    reclaim_trailing_shorthand(cond)
}

fn reclaim_trailing_shorthand(cond: Expr) -> Option<(Expr, BlockExpr)> {
    match cond {
        // An empty brace group that was greedily parsed as a struct-literal head
        // on the right spine of a binary/unary condition (`if d > f(args) {}`,
        // `if d > recv.f(args) {}`) is the control-flow body, not a literal. Peel
        // it to the underlying head plus an empty body, mirroring the top-level
        // empty case in `reclaim_as_condition_and_body`. This completes the
        // reclaim so a qualified inline-ctor-shaped method call in a bare
        // condition parses identically to the plain method call it is (RUE-951).
        Expr::StructLit(lit) if lit.fields.is_empty() => {
            let body = empty_body(&lit);
            Some((reclaim_head(lit)?, body))
        }
        Expr::StructLit(lit) if lit.fields.len() == 1 && lit.fields[0].shorthand => {
            let body = BlockExpr {
                statements: Vec::new(),
                expr: Box::new(Expr::Ident(lit.fields[0].name)),
                span: Span::with_file(lit.span.file_id, lit.name.span.end, lit.span.end),
            };
            Some((reclaim_head(lit)?, body))
        }
        Expr::Binary(mut binary) => {
            let (right, body) = reclaim_trailing_shorthand(*binary.right)?;
            binary.right = Box::new(right);
            binary.span = binary.left.span().extend_to(binary.right.span().end);
            Some((Expr::Binary(binary), body))
        }
        Expr::Unary(mut unary) => {
            let (operand, body) = reclaim_trailing_shorthand(*unary.operand)?;
            unary.operand = Box::new(operand);
            unary.span = unary.span.extend_to(unary.operand.span().end);
            Some((Expr::Unary(unary), body))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryExpr, BinaryOp, FieldInit, Ident, IntLit};
    use lasso::{Key, Spur};
    use rue_span::FileId;

    fn span(start: u32, end: u32) -> Span {
        Span::with_file(FileId::new(7), start, end)
    }

    fn ident(index: usize, start: u32) -> Ident {
        Ident {
            name: Spur::try_from_usize(index).unwrap(),
            span: span(start, start + 1),
        }
    }

    fn literal(fields: Vec<FieldInit>) -> StructLitExpr {
        StructLitExpr {
            base: None,
            name: ident(0, 4),
            ctor_args: None,
            fields,
            span: span(4, 9),
        }
    }

    #[test]
    fn empty_literal_becomes_head_and_empty_body() {
        let (head, body) = reclaim_as_condition_and_body(Expr::StructLit(literal(vec![]))).unwrap();
        assert!(matches!(head, Expr::Ident(_)));
        assert!(matches!(*body.expr, Expr::Unit(_)));
        assert_eq!(body.span, span(5, 9));
    }

    #[test]
    fn shorthand_is_peeled_from_binary_right_spine() {
        let field_name = ident(1, 7);
        let tail = Expr::StructLit(literal(vec![FieldInit {
            name: field_name,
            value: Box::new(Expr::Ident(field_name)),
            shorthand: true,
            span: span(7, 8),
        }]));
        let cond = Expr::Binary(BinaryExpr {
            left: Box::new(Expr::Int(IntLit {
                value: 1,
                span: span(0, 1),
            })),
            op: BinaryOp::Lt,
            right: Box::new(tail),
            span: span(0, 9),
        });

        let (cond, body) = reclaim_as_condition_and_body(cond).unwrap();
        assert!(matches!(cond, Expr::Binary(_)));
        assert!(matches!(*body.expr, Expr::Ident(name) if name == field_name));
        assert_eq!(cond.span(), span(0, 5));
    }

    #[test]
    fn explicit_fields_are_not_reclaimed() {
        let name = ident(1, 7);
        let explicit = literal(vec![FieldInit {
            name,
            value: Box::new(Expr::Int(IntLit {
                value: 1,
                span: span(8, 9),
            })),
            shorthand: false,
            span: span(7, 9),
        }]);
        assert!(reclaim_as_condition_and_body(Expr::StructLit(explicit)).is_none());

        // A module-qualified inline-ctor head with explicit fields is likewise
        // an ambiguous struct literal in a bare condition, so it is not
        // reclaimed and the diagnostic points the user at parentheses.
        let mut qualified_explicit = literal(vec![FieldInit {
            name,
            value: Box::new(Expr::Int(IntLit {
                value: 1,
                span: span(8, 9),
            })),
            shorthand: false,
            span: span(7, 9),
        }]);
        qualified_explicit.base = Some(Box::new(Expr::Ident(ident(2, 0))));
        qualified_explicit.ctor_args = Some(vec![]);
        assert!(reclaim_as_condition_and_body(Expr::StructLit(qualified_explicit)).is_none());
    }

    #[test]
    fn empty_qualified_ctor_head_reclaims_to_method_call() {
        // `for _ in m.f() {}` greedily parses the empty body as a qualified
        // inline-ctor head; in control-flow position it reclaims to the method
        // call plus an empty body (RUE-951).
        let mut qualified_ctor = literal(vec![]);
        qualified_ctor.base = Some(Box::new(Expr::Ident(ident(2, 0))));
        qualified_ctor.ctor_args = Some(vec![]);
        assert!(matches!(
            reclaim_empty_struct_lit_head(qualified_ctor),
            Some(Expr::MethodCall(_))
        ));
    }
}
