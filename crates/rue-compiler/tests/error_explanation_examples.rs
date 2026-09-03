use rue_compiler::{CompileOptions, SourceSnapshot, compile_snapshot};
use rue_error::{ErrorCodeExampleOutcome, error_code_explanation, error_code_metadata};

#[test]
fn compiler_owned_explanation_examples_have_the_declared_outcome() {
    for metadata in error_code_metadata()
        .iter()
        .filter(|metadata| (200..=211).contains(&metadata.code.0))
    {
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
                    assert_eq!(
                        codes.as_slice(),
                        [metadata.code],
                        "{} example {:?} must emit exactly its owning code; emitted {:?}",
                        metadata.code,
                        example.title,
                        codes,
                    );
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
