//! Pure target-descriptor fact mapping.

use super::projection::*;
use super::*;

/// The canonical pure target-descriptor kernel used by durable semantic
/// authorities.  It receives only decomposed target facts, so tests and
/// future query adapters can cover data models not currently exposed by a
/// concrete compiler target without copying the mapping policy.
pub(crate) fn resolve_target_intrinsic_facts(
    intrinsic: ComptimeTargetIntrinsic,
    argument_count: usize,
    arch: rue_target::Arch,
    os: rue_target::Os,
    data_model: rue_target::DataModel,
) -> Result<TargetEnumValue, SemanticNucleusFailure> {
    if argument_count != 0 {
        return Err(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::IntrinsicWrongArgCount {
                name: intrinsic.as_str().to_owned(),
                expected: 0,
                found: argument_count,
            },
        ));
    }
    let (type_name, variant) = match intrinsic {
        ComptimeTargetIntrinsic::Arch => (
            "Arch",
            match arch {
                rue_target::Arch::X86_64 => "X86_64",
                rue_target::Arch::Aarch64 => "Aarch64",
            },
        ),
        ComptimeTargetIntrinsic::Os => (
            "Os",
            match os {
                rue_target::Os::Linux => "Linux",
                rue_target::Os::Macos => "Macos",
            },
        ),
        ComptimeTargetIntrinsic::DataModel => (
            "DataModel",
            match data_model {
                rue_target::DataModel::Ilp32 => "Ilp32",
                rue_target::DataModel::Lp64 => "Lp64",
                rue_target::DataModel::Llp64 => "Llp64",
            },
        ),
    };
    Ok(TargetEnumValue { type_name, variant })
}

pub(crate) fn resolve_target_enum_variant_facts(
    type_name: &str,
    variant: &str,
) -> Result<TargetEnumValue, SemanticNucleusFailure> {
    const VARIANTS: &[(&str, &[&str])] = &[
        ("Arch", &["X86_64", "Aarch64"]),
        ("Os", &["Linux", "Macos"]),
        ("DataModel", &["Ilp32", "Lp64", "Llp64"]),
    ];
    let Some((canonical_type, variants)) = VARIANTS
        .iter()
        .find(|(candidate, _)| *candidate == type_name)
    else {
        return Err(SemanticNucleusFailure::Resolution(Arc::from(
            "unknown target descriptor enum",
        )));
    };
    let Some(canonical_variant) = variants
        .iter()
        .copied()
        .find(|candidate| *candidate == variant)
    else {
        return Err(SemanticNucleusFailure::Diagnostic(
            rue_error::ErrorKind::UnknownVariant {
                enum_name: (*canonical_type).to_owned(),
                variant_name: variant.to_owned(),
            },
        ));
    };
    Ok(TargetEnumValue {
        type_name: canonical_type,
        variant: canonical_variant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_kernel_covers_all_facts_and_error_channels() {
        for arch in [rue_target::Arch::X86_64, rue_target::Arch::Aarch64] {
            for os in [rue_target::Os::Linux, rue_target::Os::Macos] {
                for data_model in [
                    rue_target::DataModel::Ilp32,
                    rue_target::DataModel::Lp64,
                    rue_target::DataModel::Llp64,
                ] {
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            ComptimeTargetIntrinsic::Arch,
                            0,
                            arch,
                            os,
                            data_model,
                        )
                        .unwrap()
                        .variant,
                        match arch {
                            rue_target::Arch::X86_64 => "X86_64",
                            rue_target::Arch::Aarch64 => "Aarch64",
                        }
                    );
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            ComptimeTargetIntrinsic::Os,
                            0,
                            arch,
                            os,
                            data_model,
                        )
                        .unwrap()
                        .variant,
                        match os {
                            rue_target::Os::Linux => "Linux",
                            rue_target::Os::Macos => "Macos",
                        }
                    );
                    assert_eq!(
                        resolve_target_intrinsic_facts(
                            ComptimeTargetIntrinsic::DataModel,
                            0,
                            arch,
                            os,
                            data_model,
                        )
                        .unwrap()
                        .variant,
                        match data_model {
                            rue_target::DataModel::Ilp32 => "Ilp32",
                            rue_target::DataModel::Lp64 => "Lp64",
                            rue_target::DataModel::Llp64 => "Llp64",
                        }
                    );
                }
            }
        }
        for (type_name, variants) in [
            ("Arch", ["X86_64", "Aarch64"].as_slice()),
            ("Os", ["Linux", "Macos"].as_slice()),
            ("DataModel", ["Ilp32", "Lp64", "Llp64"].as_slice()),
        ] {
            for variant in variants {
                assert_eq!(
                    resolve_target_enum_variant_facts(type_name, variant).unwrap(),
                    TargetEnumValue { type_name, variant }
                );
            }
        }
        assert!(matches!(
            resolve_target_intrinsic_facts(
                ComptimeTargetIntrinsic::Os,
                1,
                rue_target::Arch::X86_64,
                rue_target::Os::Linux,
                rue_target::DataModel::Lp64,
            ),
            Err(SemanticNucleusFailure::Diagnostic(
                rue_error::ErrorKind::IntrinsicWrongArgCount { found: 1, .. }
            ))
        ));
        assert!(matches!(
            resolve_target_enum_variant_facts("Target", "X86_64"),
            Err(SemanticNucleusFailure::Resolution(message))
                if message.as_ref() == "unknown target descriptor enum"
        ));
        assert!(matches!(
            resolve_target_enum_variant_facts("Arch", "Linux"),
            Err(SemanticNucleusFailure::Diagnostic(rue_error::ErrorKind::UnknownVariant {
                enum_name,
                variant_name,
            })) if enum_name == "Arch" && variant_name == "Linux"
        ));
    }
}
