use rue_compiler::{CompileOptions, SourceSnapshot, compile_snapshot};
use rue_error::{ErrorCodeExampleOutcome, error_code_explanation, error_code_metadata};

#[test]
fn compiler_owned_explanation_examples_have_the_declared_outcome() {
    for metadata in error_code_metadata().iter().filter(|metadata| {
        (1..=11).contains(&metadata.code.0)
            || (100..=103).contains(&metadata.code.0)
            || (200..=211).contains(&metadata.code.0)
            || (400..=407).contains(&metadata.code.0)
            || (410..=418).contains(&metadata.code.0)
            || (419..=424).contains(&metadata.code.0)
            || (425..=433).contains(&metadata.code.0)
            || matches!(
                metadata.code.0,
                434..=437
                    | 442..=443
                    | 456..=457
                    | 461
                    | 474..=475
                    | 478
                    | 480..=497
                    | 499
                    | 600..=602
                    | 700..=702
                    | 709..=712
            )
    }) {
        let explanation = error_code_explanation(metadata.code)
            .unwrap_or_else(|| panic!("{} must have an explanation", metadata.code));
        for example in explanation.examples {
            let snapshot = SourceSnapshot::single("main.rue", example.source).unwrap();
            let result = compile_snapshot(&snapshot, &CompileOptions::default());
            match example.outcome {
                ErrorCodeExampleOutcome::EmitsThisCode => {
                    let errors = match result {
                        Ok(_) => panic!(
                            "{} example {:?} compiled successfully",
                            metadata.code, example.title
                        ),
                        Err(errors) => errors,
                    };
                    let codes = errors
                        .iter()
                        .map(|error| error.kind.code())
                        .collect::<Vec<_>>();
                    if metadata.code == rue_error::ErrorCode::LEXER_DIAGNOSTICS_OMITTED {
                        assert_eq!(codes.len(), 101);
                        assert!(
                            codes[..100]
                                .iter()
                                .all(|code| *code == rue_error::ErrorCode::UNEXPECTED_CHARACTER)
                        );
                        assert_eq!(codes[100], metadata.code);
                    } else if metadata.code == rue_error::ErrorCode::PARSER_DIAGNOSTICS_OMITTED {
                        assert_eq!(codes.len(), 101);
                        assert!(
                            codes[..100]
                                .iter()
                                .all(|code| *code == rue_error::ErrorCode::UNEXPECTED_TOKEN)
                        );
                        assert_eq!(codes[100], metadata.code);
                    } else {
                        assert_eq!(
                            codes.as_slice(),
                            [metadata.code],
                            "{} example {:?} must emit exactly its owning code; emitted {:?}: {errors:?}",
                            metadata.code,
                            example.title,
                            codes,
                        );
                    }
                }
                ErrorCodeExampleOutcome::Compiles => {
                    result.unwrap_or_else(|errors| {
                        panic!(
                            "{} example {:?} unexpectedly emitted {:?}",
                            metadata.code,
                            example.title,
                            errors
                                .iter()
                                .map(|error| error.kind.code())
                                .collect::<Vec<_>>()
                        )
                    });
                }
            }
        }
    }
}
