//! Exact model-gap inventory for the expanded `rue-spec` corpus.

use super::{InventoryScope, ModelGapAudit, ModelGapRegistration};
use rue_oracle::{
    ExternalDependencyKind, ModelGapKind, SemanticGapKind, UnsupportedIntrinsicKind,
    UnsupportedRuntimeCallKind,
};
use std::fmt;

/// Stable spec-corpus identity. The case name is the post-template-expansion
/// name returned by `rue-test-runner`; file paths are deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CaseId {
    section: String,
    case: String,
}

impl CaseId {
    pub(crate) fn new(section: impl Into<String>, case: impl Into<String>) -> Self {
        Self {
            section: section.into(),
            case: case.into(),
        }
    }
}

impl fmt::Display for CaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} :: {}", self.section, self.case)
    }
}

struct Entry {
    section: &'static str,
    case: &'static str,
    kind: ModelGapKind,
    only_on: &'static [&'static str],
}

impl Entry {
    const fn new(
        section: &'static str,
        case: &'static str,
        kind: ModelGapKind,
        only_on: &'static [&'static str],
    ) -> Self {
        Self {
            section,
            case,
            kind,
            only_on,
        }
    }

    fn registration(&self) -> ModelGapRegistration<CaseId> {
        ModelGapRegistration::new(
            CaseId::new(self.section, self.case),
            self.kind,
            self.only_on.iter().copied(),
        )
    }
}

/// The complete accepted spec oracle-debt inventory across every supported
/// host. Entries are generated from production typed diagnostics and reviewed
/// against the authoritative, post-template-expansion corpus.
const ENTRIES: &[Entry] = &[
    // Source-defined StrBuf methods expose their first unmodeled ordinary
    // projection, allocation, pointer, or inout operation to the oracle.
    Entry::new(
        "expressions.intrinsics",
        "dbg_string_borrows_not_consumed",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "dbg_string_dropped_exactly_once",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "items.general",
        "fn_builtin_method_name_allowed",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.destructors",
        "codegen_nontrivial_drop_call",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.destructors",
        "struct_with_destructor_field",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.destructors",
        "struct_with_string_fields_dropped",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.destructors",
        "type_with_destructor",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_building_message",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_byte_semantics",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_equality_after_mutation",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_push_byte",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_push_str_basic",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_push_str_multiple",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_empty_string_yields_none",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_basic",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_boundary",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_leading_zeros",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_max",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_min",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_negative",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_string_not_consumed",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i32_zero",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i64_basic",
        intrinsic(UnsupportedIntrinsicKind::ParseI64),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_i64_negative",
        intrinsic(UnsupportedIntrinsicKind::ParseI64),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_invalid_char_yields_none",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_negative_unsigned_yields_none",
        intrinsic(UnsupportedIntrinsicKind::ParseU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_overflow_i32_yields_none",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_overflow_u32_yields_none",
        intrinsic(UnsupportedIntrinsicKind::ParseU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_u32_basic",
        intrinsic(UnsupportedIntrinsicKind::ParseU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "parse_u64_basic",
        intrinsic(UnsupportedIntrinsicKind::ParseU64),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u32_entropy_failure_behavior",
        external(ExternalDependencyKind::RandomU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u32_in_range_expression",
        external(ExternalDependencyKind::RandomU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u32_multiple_calls",
        external(ExternalDependencyKind::RandomU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u32_no_args",
        external(ExternalDependencyKind::RandomU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u32_returns_u32",
        external(ExternalDependencyKind::RandomU32),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u64_multiple_calls",
        external(ExternalDependencyKind::RandomU64),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u64_no_args",
        external(ExternalDependencyKind::RandomU64),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "random_u64_returns_u64",
        external(ExternalDependencyKind::RandomU64),
        &[],
    ),
    // std.env's process-argument/environment intrinsics read captured process
    // state, an external dependency like `@random_*` (RUE-935). Each case's
    // first such read (its `@arg_count`/`@env_count`) decides the kind.
    Entry::new(
        "expressions.intrinsics",
        "arg_count_includes_argv0",
        external(ExternalDependencyKind::ArgCount),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "arg_len_and_ptr_in_and_out_of_range",
        external(ExternalDependencyKind::ArgCount),
        &[],
    ),
    Entry::new(
        "expressions.intrinsics",
        "env_len_and_ptr_out_of_range",
        external(ExternalDependencyKind::EnvCount),
        &[],
    ),
    Entry::new(
        "expressions.try",
        "try_parse_freestanding_no_imports",
        intrinsic(UnsupportedIntrinsicKind::ParseI64),
        &[],
    ),
    Entry::new(
        "expressions.try",
        "try_parse_intrinsic_none_short_circuits",
        intrinsic(UnsupportedIntrinsicKind::ParseI64),
        &[],
    ),
    Entry::new(
        "expressions.try",
        "try_parse_intrinsic_some",
        intrinsic(UnsupportedIntrinsicKind::ParseI64),
        &[],
    ),
    // ADR-0052 phase 7 (RUE-978): the unaligned-access round trip allocates
    // through @alloc, which the oracle model does not model.
    Entry::new(
        "runtime.syscall",
        "syscall_basic_aarch64",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_basic_x86_64",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_checked_block_expression",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_checked_block_expression_aarch64",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_error_is_negative_aarch64_macos",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-macos"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_max_args",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_returns_i64",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "runtime.syscall",
        "syscall_returns_i64_aarch64",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux"],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_growth_on_append",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_reserve",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "types.strings",
        "print_borrows_argument_string",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "types.strings",
        "print_empty_writes_nothing_println_empty_writes_newline",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "types.strings",
        "print_writes_bytes_without_newline",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "types.strings",
        "println_appends_single_newline",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "types.strings",
        "println_composes_with_to_string_and_concat",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
];

pub(crate) fn audit(scope: InventoryScope) -> ModelGapAudit<CaseId> {
    ModelGapAudit::new(
        "rue-spec",
        scope,
        ENTRIES.iter().map(Entry::registration),
        render_registration,
    )
}

fn render_registration(identity: &CaseId, kind: ModelGapKind, only_on: &[String]) -> String {
    let only_on = if only_on.is_empty() {
        "&[]".to_string()
    } else {
        format!(
            "&[{}]",
            only_on
                .iter()
                .map(|platform| format!("{platform:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "        Entry::new({:?}, {:?}, {}, {}),",
        identity.section,
        identity.case,
        render_kind(kind),
        only_on
    )
}

fn render_kind(kind: ModelGapKind) -> String {
    match kind {
        ModelGapKind::Semantic(SemanticGapKind::Intrinsic(kind)) => {
            format!("intrinsic(UnsupportedIntrinsicKind::{kind:?})")
        }
        ModelGapKind::Semantic(SemanticGapKind::RuntimeCall(kind)) => {
            format!("runtime_call(UnsupportedRuntimeCallKind::{kind:?})")
        }
        ModelGapKind::Semantic(SemanticGapKind::FlattenedParameterSlot) => {
            "semantic(SemanticGapKind::FlattenedParameterSlot)".to_string()
        }
        ModelGapKind::Semantic(SemanticGapKind::TextProjectionRead) => {
            "semantic(SemanticGapKind::TextProjectionRead)".to_string()
        }
        ModelGapKind::ExternalDependency(kind) => {
            format!("external(ExternalDependencyKind::{kind:?})")
        }
        ModelGapKind::ImplementationDefined(kind) => {
            format!("implementation_defined(ImplementationDefinedKind::{kind:?})")
        }
    }
}

const fn intrinsic(kind: UnsupportedIntrinsicKind) -> ModelGapKind {
    ModelGapKind::Semantic(SemanticGapKind::Intrinsic(kind))
}

const fn runtime_call(kind: UnsupportedRuntimeCallKind) -> ModelGapKind {
    ModelGapKind::Semantic(SemanticGapKind::RuntimeCall(kind))
}

const fn external(kind: ExternalDependencyKind) -> ModelGapKind {
    ModelGapKind::ExternalDependency(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_unknown_entry_uses_expanded_spec_identity_and_exact_scope() {
        assert_eq!(
            render_registration(
                &CaseId::new("runtime.syscall", "syscall_basic_aarch64"),
                external(ExternalDependencyKind::SystemCall),
                &["aarch64-linux".to_string()],
            ),
            concat!(
                "        Entry::new(\"runtime.syscall\", \"syscall_basic_aarch64\", ",
                "external(ExternalDependencyKind::SystemCall), &[\"aarch64-linux\"]),"
            )
        );
    }

    #[test]
    fn registry_identity_is_section_and_expanded_case_name() {
        assert_eq!(
            CaseId::new("types.integers", "u8_overflow_addition").to_string(),
            "types.integers :: u8_overflow_addition"
        );
    }
}
