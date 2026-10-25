use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// `fn f(self, x: u8, ...)`: patterns with types, an optional leading
    /// self parameter, and C variadics.
    Definition,
    /// `fn(u8, name: u8, ...)`: types, optionally named.
    Pointer,
    /// `Fn(u8, u16)`: bare types.
    TraitSugar,
}

pub(super) fn param_list_fn_def(p: &mut Parser<'_, '_>) {
    list(p, Flavor::Definition);
}

pub(super) fn param_list_fn_ptr(p: &mut Parser<'_, '_>) {
    list(p, Flavor::Pointer);
}

pub(super) fn param_list_fn_trait(p: &mut Parser<'_, '_>) {
    list(p, Flavor::TraitSugar);
}

fn list(p: &mut Parser<'_, '_>, flavor: Flavor) {
    let m = p.start();
    p.group(|p| {
        let mut first = true;
        comma_separated(p, |p| {
            let param = p.start();
            attributes::outer_attrs(p);
            if first && flavor == Flavor::Definition && at_self_param(p) {
                self_param(p, param);
            } else {
                self::param(p, param, flavor);
            }
            first = false;
        });
    });
    m.complete(p, PARAM_LIST);
}

fn at_self_param(p: &Parser<'_, '_>) -> bool {
    match p.current() {
        T![self] => !p.nth_at(1, T![::]),
        T![mut] => p.nth(1) == T![self],
        T![&] => match p.nth(1) {
            T![self] => true,
            T![mut] => p.nth(2) == T![self],
            LIFETIME_IDENT => p.nth(2) == T![self] || (p.nth(2) == T![mut] && p.nth(3) == T![self]),
            _ => false,
        },
        _ => false,
    }
}

/// rustc's test for a named parameter: an identifier, optionally after `&`,
/// `&&` or `mut`, followed by `:`.
fn is_named_param(p: &Parser<'_, '_>) -> bool {
    let offset = if p.at(T![&&]) {
        2
    } else if p.at(T![&]) || p.at(T![mut]) {
        1
    } else {
        0
    };
    let ident = matches!(p.nth(offset), IDENT | T![_]) || p.nth(offset).is_keyword();
    ident && p.nth_at(offset + 1, T![:])
}

fn self_param(p: &mut Parser<'_, '_>, m: Marker) {
    if p.eat(T![&]) {
        if p.at(LIFETIME_IDENT) {
            lifetime(p);
        }
        p.eat(T![mut]);
        p.bump(T![self]);
    } else {
        p.eat(T![mut]);
        p.bump(T![self]);
        if p.eat(T![:]) {
            types::type_(p);
        }
    }
    m.complete(p, SELF_PARAM);
}

fn param(p: &mut Parser<'_, '_>, m: Marker, flavor: Flavor) {
    if p.at(T![...]) {
        p.bump(T![...]);
        m.complete(p, PARAM);
        return;
    }
    match flavor {
        // 2015 trait methods may leave parameters unnamed: `fn f(&self, u8)`.
        // rustc's lookahead decides; trait and impl bodies share a grammar
        // here, so the 2015 rule applies to every function.
        Flavor::Definition if p.edition() == crate::Edition::Edition2015 && !is_named_param(p) => {
            types::type_(p);
        }
        Flavor::Definition => {
            if !p.at_ts(patterns::PATTERN_FIRST) {
                m.abandon(p);
                p.err_recover("expected a parameter", TokenSet::new(&[T![,]]));
                return;
            }
            patterns::pattern(p);
            if p.eat(T![:]) {
                if !p.eat(T![...]) {
                    types::type_(p);
                }
            } else {
                p.error("missing type for function parameter");
            }
        }
        Flavor::Pointer => {
            let named = matches!(p.current(), IDENT | T![_]) && p.nth_at(1, T![:]);
            if named {
                patterns::pattern_single(p);
                p.bump(T![:]);
            }
            types::type_(p);
        }
        Flavor::TraitSugar => types::type_(p),
    }
    m.complete(p, PARAM);
}
