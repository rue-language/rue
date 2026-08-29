#[allow(unused_macros)]
macro_rules! register_provider_probe {
    ($runtime:ident) => {{
        $runtime
            .family_with_equality(
                "compiler.body-fact-provider-probe",
                BODY_QUERY_MEMO_RETENTION,
                |left: &ProviderProbeValue, right: &ProviderProbeValue| left == right,
            )
            .expect("the provider-probe family has one canonical name")
    }};
}
