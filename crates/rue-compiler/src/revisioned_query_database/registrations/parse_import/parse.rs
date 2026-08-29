macro_rules! register_parse_import_parse {
    ($runtime:ident) => {{
        $runtime
            .content_addressed_family_with_equality(
                "compiler.parse",
                crate::session::ParseQuery::MAX_TERMINALS,
                record_equal::<crate::session::ParseQuery>,
            )
            .expect("the Parse family has one canonical name")
    }};
}
