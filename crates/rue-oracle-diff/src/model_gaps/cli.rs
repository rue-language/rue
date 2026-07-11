//! Exact model-gap inventory for the `rue-cli-tests` corpus.

use super::{InventoryScope, ModelGapAudit, ModelGapRegistration};
use rue_oracle::{
    ExternalDependencyKind, ImplementationDefinedKind, ModelGapKind, SemanticGapKind,
    UnsupportedIntrinsicKind, UnsupportedRuntimeCallKind,
};
use std::fmt;

/// Stable CLI corpus identity. File paths are deliberately excluded: cases
/// may move without resetting debt, while section/case renames must update it.
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

/// The complete accepted CLI oracle-debt inventory.
///
/// Entries are generated from unknown-gap diagnostics emitted by the real
/// production classifier, then reviewed into this typed list. Dynamic
/// `Unsupported::detail` text is intentionally absent from the policy key.
const ENTRIES: &[Entry] = &[
    Entry::new(
        "array_literal_string",
        "array_of_string_with_capacity",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "aos_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "arr_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "enum_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "nest_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "s1_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "s2_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "s3_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_abi_matrix",
        "s8_ptr_rw",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.aggregate_ascending_layout",
        "heap_multislot_roundtrip_no_clobber",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.aggregate_ascending_layout",
        "stack_raw_ptr_roundtrip",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.anon_struct_destructor",
        "generic_heap_buffer_auto_freed",
        intrinsic(UnsupportedIntrinsicKind::IntToPointer),
        &[],
    ),
    Entry::new(
        "cli.builtin_method_infer",
        "capacity_compare_literal_compiles",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "cli.byref_params",
        "inout_forwarding_still_works",
        semantic(SemanticGapKind::InoutParameterForwarding),
        &[],
    ),
    Entry::new(
        "cli.comptime_type_var_composite",
        "pointer_to_comptime_type_var",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.differential_opt",
        "raw_pointer_place_operands_across_opt_levels",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.for_loops",
        "for_chars_invalid_utf8_traps",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "cli.heap_intrinsics",
        "alloc_count_size_overflow_traps",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.heap_intrinsics",
        "alloc_page_rounding_overflow_returns_null",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.heap_intrinsics",
        "alloc_write_read_free",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.heap_intrinsics",
        "free_from_destructor",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.heap_intrinsics",
        "realloc_count_size_overflow_traps",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.heap_intrinsics",
        "realloc_grows_and_preserves",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.method_receiver_byref_places",
        "inout_self_through_inout_param",
        semantic(SemanticGapKind::InoutParameterForwarding),
        &[],
    ),
    Entry::new(
        "cli.offset_of_field_ptr",
        "field_ptr_on_param_struct",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "cli.offset_of_field_ptr",
        "field_ptr_read_roundtrip",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "cli.offset_of_field_ptr",
        "field_ptr_write_updates_field",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "cli.offset_of_field_ptr",
        "offset_of_ptr_offset_agrees_with_field_ptr",
        intrinsic(UnsupportedIntrinsicKind::FieldPointer),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "array_index_and_ptr_offset_agree",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_across_function",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_int_roundtrip",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_offset_forward_is_higher_address",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_offset_forward_on_mmap_pointer",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_offset_negative_walks_backward",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_offset_reads_offset_element",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_offset_write_hits_offset_element",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_read_roundtrip",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_write_literal_infers_i64_pointee",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_write_literal_infers_u8_pointee",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "ptr_write_mutates_local",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "raw_mut_of_parameter_mutates_it",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "raw_of_local_still_works",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "raw_of_param_struct_field",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.pointers",
        "raw_of_parameter_takes_address",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.print",
        "print_borrows_string_reusable_after_call",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "cli.print",
        "print_empty_and_println_empty",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "cli.print",
        "print_no_trailing_newline",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "cli.print",
        "print_utf8_bytes_verbatim",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.print",
        "println_adds_single_newline",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.print",
        "println_composed_with_to_string_and_concat",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.ptr_read_write_aggregate",
        "ptr_read_result_type_match_i32",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.ptr_read_write_aggregate",
        "ptr_read_two_field_struct",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.ptr_read_write_aggregate",
        "ptr_read_write_nested_struct",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.ptr_read_write_aggregate",
        "ptr_read_write_scalar",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.ptr_read_write_aggregate",
        "ptr_write_two_field_struct",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.raw_ptr_and_method_name",
        "raw_mut_ptr_does_not_suppress_destructor",
        intrinsic(UnsupportedIntrinsicKind::RawMutableAddress),
        &[],
    ),
    Entry::new(
        "cli.raw_ptr_and_method_name",
        "raw_ptr_does_not_suppress_destructor",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.raw_ptr_and_method_name",
        "raw_ptr_operand_usable_after",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.raw_ptr_and_method_name",
        "raw_ptr_then_move_operand_single_drop",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_constant_index",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_empty_array_len_zero",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_forwarding",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_index_out_of_bounds_traps",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_len",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_negative_signed_index_traps",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_sum_len_index",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_u8_element",
        intrinsic(UnsupportedIntrinsicKind::RawAddress),
        &[],
    ),
    Entry::new(
        "cli.str_fixed_type",
        "str_fixed_len_and_first_byte",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_fixed_type",
        "str_fixed_len_independent_of_capacity",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_fixed_type",
        "str_fixed_packed_byte_indexing",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_fixed_type",
        "str_fixed_param_roundtrip",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_fixed_type",
        "str_fixed_read_through_borrow_str_view",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "buffer_read_through_borrow_str_view_ok",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "str_first_class_return_store_reassign",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "str_inout_accepts_local_buffer",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "str_len_and_first_byte",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "str_packed_byte_indexing",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "str_param_literal_and_forward",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "strbuf_field_borrow_str_view",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "strbuf_literal_binding_borrow_str_len",
        semantic(SemanticGapKind::TextProjectionRead),
        &[],
    ),
    Entry::new(
        "cli.str_type",
        "strbuf_read_through_borrow_str_view_values",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.strbuf_library",
        "strbuf_new_push_str_len_print",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.strbuf_library",
        "strbuf_with_capacity_concat",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.string_alloc_failure",
        "with_capacity_huge_no_segfault",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "cli.string_alloc_failure",
        "with_capacity_normal_reserves",
        implementation_defined(ImplementationDefinedKind::StringCapacityValue),
        &[],
    ),
    Entry::new(
        "cli.string_byte_access",
        "substring_byte_range",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "cli.string_byte_access",
        "substring_out_of_range_traps",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "cli.string_byte_access",
        "substring_result_is_mutable",
        runtime_call(UnsupportedRuntimeCallKind::StringSubstring),
        &[],
    ),
    Entry::new(
        "cli.string_phase1",
        "concat_chained_and_mutable_result",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.string_phase1",
        "concat_is_concatenation_not_addition",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.string_phase1",
        "contains_and_starts_with",
        runtime_call(UnsupportedRuntimeCallKind::StringContains),
        &[],
    ),
    Entry::new(
        "cli.string_phase1",
        "to_string_all_integer_widths",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.string_phase1",
        "to_string_i32_no_cast_needed",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.try_operator",
        "try_string_payload",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.try_operator",
        "try_unwrap_and_short_circuit",
        runtime_call(UnsupportedRuntimeCallKind::StringConcat),
        &[],
    ),
    Entry::new(
        "cli.wildcard_payload_binding",
        "discarded_non_copy_payload_drops_once",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
];

pub(crate) fn audit(scope: InventoryScope) -> ModelGapAudit<CaseId> {
    ModelGapAudit::new(
        "rue-cli-tests",
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
    fn rendered_unknown_entry_is_directly_pasteable_registry_syntax() {
        assert_eq!(
            render_registration(
                &CaseId::new("cli.example", "random"),
                external(ExternalDependencyKind::RandomU32),
                &["x86-64-linux".to_string()],
            ),
            concat!(
                "        Entry::new(\"cli.example\", \"random\", ",
                "external(ExternalDependencyKind::RandomU32), &[\"x86-64-linux\"]),"
            )
        );
    }

    #[test]
    fn registry_identity_is_section_and_expanded_case_name() {
        assert_eq!(
            CaseId::new("cli.example", "case[expanded]").to_string(),
            "cli.example :: case[expanded]"
        );
    }
}
