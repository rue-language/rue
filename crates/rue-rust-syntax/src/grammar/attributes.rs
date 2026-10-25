use super::*;

pub(super) fn inner_attrs(p: &mut Parser<'_, '_>) {
    while p.at(T![#]) && p.nth(1) == T![!] {
        attr(p, true);
    }
}

pub(super) fn outer_attrs(p: &mut Parser<'_, '_>) {
    while p.at(T![#]) {
        attr(p, false);
    }
}

fn attr(p: &mut Parser<'_, '_>, inner: bool) {
    let m = p.start();
    p.bump(T![#]);
    if inner {
        p.bump(T![!]);
    } else if p.at(T![!]) {
        p.error("an inner attribute is not permitted here");
        p.bump(T![!]);
    }
    if p.at(T!['[']) {
        p.group(meta);
    } else {
        p.error("expected `[`");
    }
    m.complete(p, ATTR);
}

/// The inside of `#[...]`: `path`, `path(tokens)`, `path = expr`, or the
/// 2024 `unsafe(meta)` wrapper.
fn meta(p: &mut Parser<'_, '_>) {
    let m = p.start();
    if p.at(T![unsafe]) && p.nth(1) == T!['('] {
        p.bump(T![unsafe]);
        p.group(meta);
        m.complete(p, META);
        return;
    }
    paths::attr_path(p);
    match p.current() {
        T![=] => {
            p.bump(T![=]);
            if expressions::expr(p).is_none() {
                p.error("expected an expression");
            }
        }
        T!['('] | T!['['] | T!['{'] => token_tree(p),
        _ => {}
    }
    m.complete(p, META);
}
