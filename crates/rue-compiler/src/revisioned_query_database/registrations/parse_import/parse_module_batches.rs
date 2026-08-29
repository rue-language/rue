macro_rules! register_parse_import_parse_module_batches {
    ($parse_modules_for_batch:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.parse-module-frontier",
                1,
                |left: &ParseModuleBatchValue, right: &ParseModuleBatchValue| {
                    left.0.len() == right.0.len()
                        && left
                            .0
                            .iter()
                            .zip(right.0.iter())
                            .all(|(left, right)| parse_module_value_equal(left, right))
                },
                move |context, _, key: &ParseModuleBatchKey| {
                    context.record_work(rue_query::WorkItem::new(
                        "parse.frontier.items",
                        key.modules.len() as u64,
                    ));
                    context.record_work(rue_query::WorkItem::new("parse.frontier.batches", 1));
                    context.record_work(rue_query::WorkItem::new("parse.frontier.overhead", 1));
                    let _attempts = context.retain_nested_attempts_for(&["compiler.parse-module"]);
                    let terminals = context.query_registered_adaptive_batch_refs(
                        &$parse_modules_for_batch,
                        key.modules.iter(),
                    )?;
                    let values = terminals
                        .iter()
                        .map(|terminal| {
                            let rue_query::QueryOutcome::Success(value) = terminal.outcome() else {
                                unreachable!("ParseModule publishes typed values")
                            };
                            value.clone()
                        })
                        .collect::<Vec<_>>();
                    let kind = if values.iter().all(|value| value.result.is_ok()) {
                        QueryTerminalKind::Success
                    } else {
                        QueryTerminalKind::Failure
                    };
                    Ok(QueryOutput::success(ParseModuleBatchValue(values.into()))
                        .with_terminal_kind(kind))
                },
            )
            .expect("the ParseModuleFrontier family has one canonical name")
    }};
}
