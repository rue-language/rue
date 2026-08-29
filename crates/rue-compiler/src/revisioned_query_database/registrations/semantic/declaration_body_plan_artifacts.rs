macro_rules! register_semantic_declaration_body_plan_artifacts {
    ($astgen_evaluations_for_artifacts:ident, $index_for_declaration_body_plan_artifacts:ident, $parse_for_declaration_body_plan_artifacts:ident, $plan_failure_injection_for_artifacts:ident, $runtime:ident) => {{
$runtime
            .family_with_equality_and_evaluator_and_retained_charge(
                "compiler.declaration-body-plan-artifacts",
                BODY_QUERY_MEMO_RETENTION,
                declaration_body_plan_artifacts_equal,
                DeclarationBodyPlanArtifactsValue::retained_charge,
                move |context, _, key: &DeclarationBodyPlanQueryKey| {
                    context.check_canceled()?;
                    #[cfg(test)]
                    if let Some((candidate, errors)) = $plan_failure_injection_for_artifacts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                        && candidate == &key.0
                    {
                        return Ok(QueryOutput::success(
                            DeclarationBodyPlanArtifactsValue::Failure(
                                DeclarationBodyPlanFailure::CandidateRirRejected(errors.clone()),
                            ),
                        )
                        .with_terminal_kind(QueryTerminalKind::Failure));
                    }
                    let parsed = context.query_registered(
                        &$parse_for_declaration_body_plan_artifacts,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let indexed = context.query_registered(
                        &$index_for_declaration_body_plan_artifacts,
                        ModuleQueryKey(key.0.module.clone()),
                    )?;
                    let rue_query::QueryOutcome::Success(parsed) = parsed.outcome() else {
                        unreachable!("ParseModule publishes typed values")
                    };
                    let rue_query::QueryOutcome::Success(indexed) = indexed.outcome() else {
                        unreachable!("ModuleIndex publishes typed values")
                    };
                    let mut structural_work = Vec::new();
                    let value = match (&parsed.result, &indexed.0) {
                        (Err(errors), _) | (_, Err(errors)) => DeclarationBodyPlanArtifactsValue::Failure(
                            DeclarationBodyPlanFailure::CandidateRirRejected(errors.clone()),
                        ),
                        (Ok(module), Ok(_)) => {
                            #[cfg(test)]
                            $astgen_evaluations_for_artifacts.fetch_add(
                                1,
                                std::sync::atomic::Ordering::Relaxed,
                            );
                            match crate::canonical_lower::lower_parsed_declaration_body_plan(
                                module,
                                &key.0,
                                || context.check_canceled(),
                            ) {
                            Ok(artifacts) => {
                                let instructions = artifacts.plan.instruction_count();
                                structural_work.push(WorkItem::new(
                                    "candidate_body_plan.construction.plans",
                                    1,
                                ));
                                structural_work.push(WorkItem::new(
                                    "candidate_body_plan.construction.instructions",
                                    instructions as u64,
                                ));
                                structural_work.push(WorkItem::new(
                                    "candidate_body_plan.construction.payload_words",
                                    artifacts.plan.payload_word_count() as u64,
                                ));
                                DeclarationBodyPlanArtifactsValue::Available(Arc::new(artifacts))
                            }
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::Query(
                                abort,
                            )) => return Err(abort),
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::MissingCandidate) => {
                                DeclarationBodyPlanArtifactsValue::Failure(
                                    DeclarationBodyPlanFailure::CandidateUnavailable(key.0.clone()),
                                )
                            }
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::ForeignSymbol(detail)) => {
                                DeclarationBodyPlanArtifactsValue::Failure(
                                    DeclarationBodyPlanFailure::ForeignSymbol(detail),
                                )
                            }
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::Build(error)) => {
                                DeclarationBodyPlanArtifactsValue::Failure(
                                    DeclarationBodyPlanFailure::Build(
                                        crate::canonical_lower::rir_build_error_kind(
                                            "packed body-plan construction",
                                            &error,
                                        ),
                                    ),
                                )
                            }
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::Payload(detail)) => {
                                DeclarationBodyPlanArtifactsValue::Failure(
                                    DeclarationBodyPlanFailure::Payload(detail),
                                )
                            }
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::Validation(detail)) => {
                                DeclarationBodyPlanArtifactsValue::Failure(
                                    DeclarationBodyPlanFailure::Validation(detail),
                                )
                            }
                            Err(crate::canonical_lower::DeclarationBodyPlanBuildFailure::SpanProjection(detail)) => {
                                DeclarationBodyPlanArtifactsValue::Failure(
                                    DeclarationBodyPlanFailure::SpanProjection(detail),
                                )
                            }
                            }
                        }
                    };
                    let kind = if matches!(
                        value,
                        DeclarationBodyPlanArtifactsValue::Available(_)
                    ) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    let output = QueryOutput::success(value).with_terminal_kind(kind);
                    if kind == QueryTerminalKind::Success {
                        Ok(output.with_work(structural_work))
                    } else {
                        Ok(output)
                    }
                },
            )
            .expect("the DeclarationBodyPlanArtifacts family has one canonical name")
    }};
}
