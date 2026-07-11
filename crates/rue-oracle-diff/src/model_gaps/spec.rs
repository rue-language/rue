//! Exact model-gap inventory for the expanded `rue-spec` corpus.

use super::{InventoryScope, ModelGapAudit, ModelGapRegistration};
use rue_oracle::{
    ExternalDependencyKind, ImplementationDefinedKind, ModelGapKind, SemanticGapKind,
    UnsupportedIntrinsicKind, UnsupportedRuntimeCallKind,
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
    Entry::new(
        "expressions.comparison",
        "struct_raw_pointer_field_equality_by_address",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "expressions.for",
        "for_chars_invalid_utf8_traps",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "expressions.for",
        "for_chars_lossy_replaces_invalid",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
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
    Entry::new(
        "items.functions",
        "borrow_with_regular_param",
        semantic(SemanticGapKind::FlattenedParameterSlot),
        &[],
    ),
    Entry::new(
        "items.functions",
        "inout_array_forward",
        semantic(SemanticGapKind::InoutParameterForwarding),
        &[],
    ),
    Entry::new(
        "items.functions",
        "inout_array_with_index_variable",
        semantic(SemanticGapKind::FlattenedParameterSlot),
        &[],
    ),
    Entry::new(
        "items.functions",
        "inout_forward_struct",
        semantic(SemanticGapKind::InoutParameterForwarding),
        &[],
    ),
    Entry::new(
        "items.impl-blocks",
        "arg_reads_receiver_allowed",
        semantic(SemanticGapKind::FlattenedParameterSlot),
        &[],
    ),
    Entry::new(
        "items.impl-blocks",
        "inout_self_with_arg_and_field_writes",
        semantic(SemanticGapKind::FlattenedParameterSlot),
        &[],
    ),
    Entry::new(
        "items.unchecked",
        "ptr_ops_in_checked_ok",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "runtime.heap",
        "alloc_multiple_elements",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "runtime.heap",
        "alloc_then_free",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "runtime.heap",
        "alloc_write_read",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "runtime.heap",
        "realloc_null_is_alloc",
        intrinsic(UnsupportedIntrinsicKind::IntToPointer),
        &[],
    ),
    Entry::new(
        "runtime.heap",
        "realloc_preserves_contents",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "field_ptr_first_field",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "field_ptr_read_matches_direct",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "field_ptr_write_updates_field",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "int_to_ptr_mutable_pointer",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "int_to_ptr_u64_pointer",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "int_to_ptr_with_type_annotation",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "ptr_offset_add_backward",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "ptr_offset_add_forward",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "ptr_to_int_basic",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "ptr_write_and_read",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "ptr_write_array_element",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "raw_array_element",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "raw_array_last_element",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "raw_array_second_element",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "runtime.pointers",
        "raw_simple_local",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
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
        "string_capacity_literal",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_clear",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_growth_on_append",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_heap_promotion",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_literal_no_capacity",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_literal_read_only",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_query_borrow_valid",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_representation_len_cap",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_reserve",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.mutable_strings",
        "string_with_capacity",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "types.str_fixed_type",
        "str_fixed_coerces_to_borrow_str_param",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_fixed_type",
        "str_fixed_exact_capacity_ok",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_fixed_type",
        "str_fixed_in_struct_field",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_fixed_type",
        "str_fixed_len_is_current_not_capacity",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_fixed_type",
        "str_fixed_returnable",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "borrow_str_view_of_strbuf_reads_values",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "borrow_str_view_reads_and_reborrows",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "inout_str_accepts_local_buffer",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "str_first_class_reassign",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "str_first_class_return",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "str_first_class_struct_field",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "str_len_bytes",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "types.str_type",
        "str_len_multibyte",
        semantic(SemanticGapKind::TextProjectionRead),
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
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_concat_borrows_operands",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_concat_is_not_integer_add",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_concat_literals",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_contains_byte_level",
        runtime_call(UnsupportedRuntimeCallKind::StringContains),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_dropped_implicitly",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_heap_freed_once",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_search_does_not_consume",
        runtime_call(UnsupportedRuntimeCallKind::StringContains),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_starts_with_byte_level",
        runtime_call(UnsupportedRuntimeCallKind::StringStartsWith),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_substring_bytes",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_substring_does_not_consume",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_substring_empty",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_substring_len",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "types.strings",
        "string_substring_out_of_bounds_traps",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
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
        ModelGapKind::Semantic(SemanticGapKind::InoutParameterForwarding) => {
            "semantic(SemanticGapKind::InoutParameterForwarding)".to_string()
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

const fn semantic(kind: SemanticGapKind) -> ModelGapKind {
    ModelGapKind::Semantic(kind)
}

const fn external(kind: ExternalDependencyKind) -> ModelGapKind {
    ModelGapKind::ExternalDependency(kind)
}

const fn implementation_defined(kind: ImplementationDefinedKind) -> ModelGapKind {
    ModelGapKind::ImplementationDefined(kind)
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
