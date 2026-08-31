use super::super::tests::snapshot;
use super::*;

#[test]
fn syntax_artifact_retains_enum_directive_child() {
    let source = snapshot(
        &[(
            1,
            "/p/main.rue",
            "main.rue",
            "@non_exhaustive pub enum Color { Red, Green }",
        )],
        1,
    );
    let mut session = CompilerSession::new();
    let syntax = session.update(&source).into_result().unwrap();
    let item = syntax
        .modules()
        .next()
        .expect("the syntax view has one module")
        .nodes()
        .next()
        .expect("the module has one item");
    assert_eq!(item.kind(), "enum");
    let children = item.children().collect::<Vec<_>>();
    assert_eq!(
        children.first().map(|child| child.kind()),
        Some("directive")
    );
    assert_eq!(
        children.first().and_then(|child| child.name()),
        Some("non_exhaustive")
    );
}
