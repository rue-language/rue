macro_rules! register_parse_import_test_candidate_scans {
    ($runtime:ident, $test_candidate_store_for_scans:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.test-candidate-scan",
                MODULE_QUERY_MEMO_RETENTION,
                test_candidate_scan_value_equal,
                move |context, _, key: &TestCandidateScanKey| {
                    // One declared input: this candidate's acquired bytes or
                    // its typed absent/unreadable observation. Editing one
                    // candidate therefore re-scans exactly that candidate.
                    context.input(test_candidate_input(&key.0))?;
                    let view =
                        test_candidate_view(&$test_candidate_store_for_scans, context.revision())?;
                    let leaf = view
                        .leaves
                        .get(&key.0.runtime_input_key())
                        .ok_or(QueryAbort::Canceled)?;
                    Ok(QueryOutput::success(TestCandidateScanValue(
                        crate::test_candidates::scan_candidate_outcome(
                            leaf.identity.path(),
                            &leaf.outcome,
                        ),
                    )))
                },
            )
            .expect("the TestCandidateScan family has one canonical name")
    }};
}
