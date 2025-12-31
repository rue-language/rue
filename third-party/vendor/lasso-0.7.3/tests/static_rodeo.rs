use lasso::Rodeo;
use std::sync::RwLock;

lazy_static::lazy_static! {
    static ref INTERNER: RwLock<Rodeo> = RwLock::new(Rodeo::new());
}

#[test]
fn access_interner() {
    let key = INTERNER
        .write()
        .unwrap()
        .get_or_intern("test strings of things with rings");

    assert_eq!(
        key,
        INTERNER
            .write()
            .unwrap()
            .get_or_intern("test strings of things with rings")
    );
    assert_eq!(
        "test strings of things with rings",
        INTERNER.read().unwrap().resolve(&key)
    );
}
