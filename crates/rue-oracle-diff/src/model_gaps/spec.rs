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
    // Borrowing an EMPTY fixed array as a slice (7.2:14) materializes the
    // pointer word as `@int_to_ptr(0)` rather than `@raw(arr[0])`, because
    // `[T; 0]` has no element 0 to address. The oracle models neither that
    // conventional null pointer nor a read through it — and it never needs to:
    // every slice read is guarded by `i < len`, and `len` is 0, so the pointer
    // is unobservable. Non-empty slice cases in this section are fully modeled
    // and diffed; only the empty-view cases are debt.
    Entry::new(
        "arrays.slices",
        "slice_coercion_empty_array_is_exempt_from_narrow_restriction",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    Entry::new(
        "arrays.slices",
        "slice_index_into_empty_view_traps",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    Entry::new(
        "arrays.slices",
        "slice_len_of_empty_view_is_zero",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    // Source-defined StrBuf methods may still expose an unsupported target or
    // runtime boundary; their ordinary projection, allocation, pointer, and
    // inout representation paths are modeled.
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
    // ADR-0059 phase 5 (RUE-963): the typed-access width and unaligned round
    // trip cases anchor their pointers via @int_to_ptr, which the oracle does
    // not model.

    // ADR-0052 phase 7 (RUE-978): these cases still depend on an unaligned
    // access outcome that is outside the modeled contract; allocation and
    // typed byte access themselves are modeled by the oracle.
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
    // RUE-1614: the legal consume-before-`?` regression case exercises the
    // early-return failure edge through @parse_i64, which the oracle does
    // not model (same debt as the expressions.try entries above).
    Entry::new(
        "types.move-semantics",
        "linear_consumed_before_try_accepted",
        intrinsic(UnsupportedIntrinsicKind::ParseI64),
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
