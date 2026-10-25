use super::*;

/// How generic arguments are written in a path.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// `use` paths, attribute paths and visibilities: no generic arguments.
    Use,
    /// Type paths: `Vec<u8>`, `Fn(u8) -> u8`.
    Type,
    /// Expression and pattern paths: generic arguments need a turbofish,
    /// `Vec::<u8>::new`.
    Expr,
}

pub(super) const PATH_FIRST: TokenSet = TokenSet::new(&[
    IDENT,
    T![self],
    T![super],
    T![crate],
    T![Self],
    T![:],
    T![<],
]);

const PATH_RECOVERY: TokenSet = TokenSet::new(&[T![;], T![,], T![=], T!['('], T!['{']]);

/// Whether the current token starts a path (including a qualified path).
pub(super) fn is_path_start(p: &Parser<'_, '_>) -> bool {
    is_use_path_start(p) || p.at(T![<]) || p.at(T![Self])
}

/// Whether the `n`th token starts a path.
pub(super) fn is_path_start_at(p: &Parser<'_, '_>, n: usize) -> bool {
    match p.nth(n) {
        IDENT | T![self] | T![super] | T![crate] | T![Self] | T![<] => true,
        T![:] => p.nth_at(n, T![::]),
        _ => false,
    }
}

pub(super) fn is_use_path_start(p: &Parser<'_, '_>) -> bool {
    match p.current() {
        IDENT | T![self] | T![super] | T![crate] | T![Self] => true,
        T![:] => p.at(T![::]),
        _ => false,
    }
}

pub(super) fn use_path(p: &mut Parser<'_, '_>) -> CompletedMarker {
    path(p, Mode::Use)
}

pub(super) fn attr_path(p: &mut Parser<'_, '_>) -> CompletedMarker {
    path(p, Mode::Use)
}

pub(super) fn type_path(p: &mut Parser<'_, '_>) -> CompletedMarker {
    path(p, Mode::Type)
}

pub(super) fn expr_path(p: &mut Parser<'_, '_>) -> CompletedMarker {
    path(p, Mode::Expr)
}

fn path(p: &mut Parser<'_, '_>, mode: Mode) -> CompletedMarker {
    let m = p.start();
    path_segment(p, mode, true);
    let qualifier = m.complete(p, PATH);
    path_continue(p, qualifier, mode)
}

/// Extends a completed path with further `::segment`s.
pub(super) fn path_continue(
    p: &mut Parser<'_, '_>,
    mut qualifier: CompletedMarker,
    mode: Mode,
) -> CompletedMarker {
    loop {
        // In a use tree, `a::{...}` and `a::*` end the path.
        let use_tree_suffix = mode == Mode::Use && matches!(p.nth(2), T![*] | T!['{']);
        if !p.at(T![::]) || use_tree_suffix {
            return qualifier;
        }
        let m = qualifier.precede(p);
        p.bump(T![::]);
        path_segment(p, mode, false);
        qualifier = m.complete(p, PATH);
    }
}

fn path_segment(p: &mut Parser<'_, '_>, mode: Mode, first: bool) {
    let m = p.start();
    if first && p.at(T![<]) {
        // A qualified path: `<T>::x` or `<T as Trait>::x`.
        p.bump(T![<]);
        types::type_(p);
        if p.eat(T![as]) {
            let trait_ref = p.start();
            type_path(p);
            trait_ref.complete(p, PATH_TYPE);
        }
        p.expect(T![>]);
        if !p.at(T![::]) {
            p.error("expected `::`");
        }
    } else {
        if first && p.at(T![::]) {
            p.bump(T![::]);
        }
        match p.current() {
            IDENT => {
                name_ref(p);
                opt_path_args(p, mode);
            }
            T![self] | T![super] | T![crate] | T![Self] => {
                let name = p.start();
                p.bump_any();
                name.complete(p, NAME_REF);
                opt_path_args(p, mode);
            }
            _ => p.err_recover("expected an identifier", PATH_RECOVERY),
        }
    }
    m.complete(p, PATH_SEGMENT);
}

fn opt_path_args(p: &mut Parser<'_, '_>, mode: Mode) {
    match mode {
        Mode::Use => {}
        Mode::Type => {
            if p.at(T![::]) && p.nth(2) == T![<] {
                p.bump(T![::]);
                generic_arg_list(p);
            } else if p.at(T![<]) && !p.at(T![<=]) {
                generic_arg_list(p);
            } else if p.at(T!['(']) {
                // `Fn(A, B) -> C` sugar.
                parenthesized_args(p);
            } else if p.at(T![::]) && p.nth(2) == T!['('] {
                p.bump(T![::]);
                parenthesized_args(p);
            }
        }
        Mode::Expr => {
            if p.at(T![::]) && p.nth(2) == T![<] {
                p.bump(T![::]);
                generic_arg_list(p);
            } else if p.at(T![::]) && p.nth(2) == T!['('] {
                // `f::(A) -> B`: rustc parses parenthesized arguments in
                // expression paths too, and rejects them in a later phase
                // unless the path names an `Fn` trait.
                p.bump(T![::]);
                parenthesized_args(p);
            }
        }
    }
}

fn parenthesized_args(p: &mut Parser<'_, '_>) {
    params::param_list_fn_trait(p);
    opt_ret_type(p);
}

pub(super) fn generic_arg_list(p: &mut Parser<'_, '_>) {
    let m = p.start();
    p.bump(T![<]);
    while !p.at(T![>]) && !p.at_eof() {
        if !generic_arg(p) {
            p.err_recover(
                "expected a generic argument",
                TokenSet::new(&[T![>], T![,]]),
            );
            if !p.at(T![,]) {
                break;
            }
        }
        if !p.at(T![>]) && !p.expect(T![,]) {
            break;
        }
    }
    p.expect(T![>]);
    m.complete(p, GENERIC_ARG_LIST);
}

fn generic_arg(p: &mut Parser<'_, '_>) -> bool {
    match p.current() {
        LIFETIME_IDENT => {
            let m = p.start();
            lifetime(p);
            m.complete(p, LIFETIME_ARG);
        }
        T!['{'] | T![-] | T![true] | T![false] => {
            let m = p.start();
            expressions::const_arg(p);
            m.complete(p, CONST_ARG);
        }
        k if k.is_literal() => {
            let m = p.start();
            expressions::const_arg(p);
            m.complete(p, CONST_ARG);
        }
        // `Item = T`, `Item: Bound`, `Item<'a> = T`: an associated type
        // constraint, which starts like a type path.
        // rustc's parser treats keywords as identifiers here (`Self: Trait`
        // is rejected by a later phase), so they are accepted too.
        IDENT | T![Self] | T![self] | T![super] | T![crate]
            if matches!(p.nth(1), T![=] | T![:] | T![<])
                && !p.nth_at(1, T![::])
                && !p.nth_at(1, T![==])
                && !p.nth_at(1, T![=>]) =>
        {
            let m = p.start();
            let path_type = p.start();
            let path = p.start();
            let segment = p.start();
            let name = p.start();
            p.bump_any();
            name.complete(p, NAME_REF);
            if p.at(T![<]) {
                generic_arg_list(p);
            }
            match p.current() {
                T![=] => {
                    p.bump(T![=]);
                    if p.at_ts(types::TYPE_FIRST) {
                        types::type_(p);
                    } else {
                        expressions::const_arg(p);
                    }
                    segment.abandon(p);
                    path.abandon(p);
                    path_type.abandon(p);
                    m.complete(p, ASSOC_TYPE_ARG);
                }
                T![:] if !p.at(T![::]) => {
                    generics::bounds(p);
                    segment.abandon(p);
                    path.abandon(p);
                    path_type.abandon(p);
                    m.complete(p, ASSOC_TYPE_ARG);
                }
                _ => {
                    segment.complete(p, PATH_SEGMENT);
                    let qualifier = path.complete(p, PATH);
                    path_continue(p, qualifier, Mode::Type);
                    let ty = path_type.complete(p, PATH_TYPE);
                    types::opt_bounds_as_dyn_trait_type(p, ty);
                    m.complete(p, TYPE_ARG);
                }
            }
        }
        _ if p.at_ts(types::TYPE_FIRST) => {
            let m = p.start();
            types::type_(p);
            m.complete(p, TYPE_ARG);
        }
        _ => return false,
    }
    true
}
