macro_rules! register_body_body_toolchain_demands {
    ($artifacts_for_toolchain_demands:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.body-toolchain-demands",
                BODY_QUERY_MEMO_RETENTION,
                |left: &crate::BodyToolchainDemand, right: &crate::BodyToolchainDemand| {
                    left == right
                },
                move |context, _, key: &crate::body_query::BodyQueryKey| {
                    let Some(definition) = body_source_definition_key(&key.instance).cloned()
                    else {
                        return Ok(QueryOutput::success(
                            crate::BodyToolchainDemand::from_payload_kinds([], None, false),
                        ));
                    };
                    let Some(candidate) = declaration_candidate_for_stable_key(&definition) else {
                        return Ok(QueryOutput::success(
                            crate::BodyToolchainDemand::from_payload_kinds(
                                [],
                                Some(definition),
                                false,
                            ),
                        ));
                    };
                    let artifact = context.query_registered(
                        &$artifacts_for_toolchain_demands,
                        DeclarationBodyPlanQueryKey(candidate),
                    )?;
                    let rue_query::QueryOutcome::Success(artifact) = artifact.outcome() else {
                        unreachable!("DeclarationBodyPlanArtifacts publishes typed values")
                    };
                    let payload_kinds = match artifact {
                        DeclarationBodyPlanArtifactsValue::Available(artifact) => artifact
                            .plan
                            .fallible_intrinsics()
                            .iter()
                            .map(crate::well_known_option::FalliblePayload::from_rir)
                            .collect::<Vec<_>>(),
                        DeclarationBodyPlanArtifactsValue::Failure(_) => Vec::new(),
                    };
                    Ok(QueryOutput::success(
                        crate::BodyToolchainDemand::from_payload_kinds(
                            payload_kinds,
                            Some(definition),
                            true,
                        ),
                    ))
                },
            )
            .expect("the BodyToolchainDemands family has one canonical name")
    }};
}
