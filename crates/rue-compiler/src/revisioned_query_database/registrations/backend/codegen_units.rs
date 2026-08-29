macro_rules! register_backend_codegen_units {
    ($codegen_batch_gate_for_evaluator:ident, $codegen_gate_for_evaluator:ident, $optimized_cfg_batches_for_codegen:ident, $optimized_cfgs_for_codegen:ident, $runtime:ident) => {{
        $runtime
            .family_with_equality_and_evaluator(
                "compiler.codegen-unit",
                BODY_QUERY_MEMO_RETENTION,
                crate::codegen_query::codegen_unit_value_equal,
                move |context, _, key: &crate::codegen_query::CodegenUnitQueryKey| {
                    #[cfg(test)]
                    if let Some(gate) = $codegen_gate_for_evaluator
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        gate.evaluator_wait();
                    }
                    #[cfg(test)]
                    let batch_gate = $codegen_batch_gate_for_evaluator
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    #[cfg(test)]
                    if let Some(gate) = batch_gate {
                        gate.evaluator_wait();
                    }
                    crate::codegen_query::evaluate_codegen_unit(
                        context,
                        &$optimized_cfgs_for_codegen,
                        &$optimized_cfg_batches_for_codegen,
                        key,
                    )
                },
            )
            .expect("the CodegenUnit family has one canonical name")
    }};
}
