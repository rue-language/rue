//! Exact model-gap inventory for the `rue-cli-tests` corpus.

use super::{InventoryScope, ModelGapAudit, ModelGapRegistration};
use rue_oracle::{
    ExternalDependencyKind, ModelGapKind, SemanticGapKind, UnsupportedIntrinsicKind,
    UnsupportedRuntimeCallKind,
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
/// `Unsupported::detail` text is intentionally absent from the policy key. The
/// gap *reasons* are layout-independent (unmodeled raw-pointer / `@field_ptr`
/// intrinsics), so the compact layout (ADR-0052, RUE-987) becoming the default
/// left this inventory's reasons unchanged; only the corpus cases' observed
/// values moved, which the RUE-987 sweep re-verified.
const ENTRIES: &[Entry] = &[
    Entry::new(
        "cli.arraybuf_library",
        "arraybuf_strbuf_elements",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.arraybuf_library",
        "arraybuf_zero_sized_element",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.const_init",
        "string_const_prints_and_measures",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.enum_payloads",
        "returned_nested_json_drops_safely",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.for_loops",
        "for_chars_lossy_replaces_raw_invalid_bytes",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    // std.fs File IO v0 (RUE-712, ADR-0057): pure-Rue fs over @syscall. The
    // oracle model cannot execute the raw-pointer/syscall substrate (StrBuf/
    // ArrayBuf `@int_to_ptr` prologue, `@alloc`, `@syscall`), so every
    // case is accepted debt, exactly like the arraybuf/strbuf CLI cases. The
    // two error-detection cases run `only_on` Linux (macOS carry-flag gap,
    // ADR-0057 §3a), so their gap registration is Linux-scoped to match.
    Entry::new(
        "cli.fs_file_io",
        "fs_roundtrip",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_append",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_drop_close_reopen",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_close_then_reopen_safe",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_read_full_buffer_invalid",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_read_whole_file_loop",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_reserve_then_read_fills",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_open_missing_not_found",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_write_to_readonly",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    // std.fs v1 follow-ups (ADR-0057 "Future Work"): seek/tell, fstat/newfstatat
    // metadata, rename, unlink, and directory create/remove. Same raw-pointer
    // substrate as v0; with heap allocation and inout forwarding modeled, the
    // oracle reaches the still-unmodeled byte-copy intrinsic. These cases are
    // `only_on` the two Linux targets (macOS stat layout + Darwin *at syscall
    // numbers are a documented, unverified follow-up), so their scope is
    // Linux-only to match.
    Entry::new(
        "cli.fs_file_io",
        "fs_seek_set_read_back",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_seek_cur_end_relative",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_stat_size_and_is_file",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_metadata_by_path_size",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_rename_old_gone_new_present",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_remove_file_then_open_notfound",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_stat_is_dir_then_rmdir",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    // RUE-995: the `create_dir_all` cases reach the same `@byte_copy` gap as the
    // rest of the std.fs group — every one of them marshals a path through
    // StrBuf, which copies bytes in bulk. The directory behavior they assert is
    // covered by their exact-stdout CLI assertions, not by the oracle model.
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_then_file_roundtrip_inside",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_all_nested_levels_usable",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_double_create_flat_errs_recursive_ok",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_all_separator_shapes",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_all_file_in_the_way_errs",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &["aarch64-linux", "x86-64-linux"],
    ),
    // RUE-682: only the std.hash cases that route bytes through StrBuf or
    // ArrayBuf(u8) hit the `@byte_copy` gap. The three that hash `str` views
    // directly — including `hash_known_answer_vectors`, which carries the
    // published FNV-1a/64 vectors — ARE modeled, so the oracle differentially
    // checks the hash arithmetic itself. That is the coverage worth having here;
    // the container spellings are asserted to agree with `str` inside the cases.
    Entry::new(
        "cli.std_hash",
        "hash_chunking_does_not_change_result",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_hash",
        "hash_str_strbuf_arraybuf_agree",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_hash",
        "hash_order_and_byte_sensitive",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    // ADR-0052 phase 3 (RUE-974): the compact-layout CLI cases reach a field
    // through `@field_ptr`, which the oracle model does not model (same gap as
    // the `offset_of_field_ptr` cases above).
    // ADR-0052 phase 5.5 (RUE-989): the narrow-access execution cases reach
    // memory through `@field_ptr` and `@alloc`, both outside the oracle model
    // (same gaps as the entries above and the heap_intrinsics corpus).
    // ADR-0052 phase 5.6 (RUE-1000): the compact-enum heap round-trips reach
    // memory through `@alloc`, outside the oracle model (same gap as the narrow
    // heap cases above).
    Entry::new(
        "cli.aggregate_layout",
        "compact_enum_padding_is_deterministically_zeroed",
        intrinsic(UnsupportedIntrinsicKind::ByteWrite),
        &[],
    ),
    // ADR-0052 phase 5.10 (RUE-987): the whole compact-struct-through-pointer
    // round-trip reaches memory through `@alloc`, outside the oracle model (same
    // gap as the narrow/enum heap cases above).
    // ADR-0052 phase 5.10 (RUE-987): the std-under-gate sweep's heap cases reach
    // memory through `@alloc`, outside the oracle model. (The container dogfood is
    // multi-file and excluded upstream; the two refusal sentinels are expected
    // compile failures.)
    // ADR-0052 phase 5.10 (RUE-1014): the variant-dependent-enum-image and
    // compact-array heap round-trips reach memory through `@alloc`, outside the
    // oracle model (same gap as the enum/struct heap cases above).
    Entry::new(
        "cli.aggregate_layout",
        "compact_enum_variant_overwrite_leaves_no_residue",
        intrinsic(UnsupportedIntrinsicKind::PointerRead),
        &[],
    ),
    // ADR-0052 phase 5.12 (RUE-1037): the heterogeneous-enum tag-dispatch heap
    // round-trips reach memory through `@alloc`, outside the oracle model (same
    // gap as the enum/struct heap cases above).
    Entry::new(
        "cli.aggregate_layout",
        "compact_heterogeneous_enum_heap_overwrite_no_residue",
        intrinsic(UnsupportedIntrinsicKind::PointerRead),
        &[],
    ),
    // RUE-1014: the real-std json/priority-queue cases reach memory through the
    // std allocators (`@alloc` in StrBuf, `@int_to_ptr` in ArrayBuf.new()),
    // outside the oracle model.

    // RUE-978: the byte-surface behavior cases allocate through @alloc,
    // which the oracle model does not model.
    Entry::new(
        "cli.pointers",
        "ptr_offset_forward_on_mmap_pointer",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "cli.print",
        "print_borrows_string_reusable_after_call",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
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
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.programs",
        "dbg_string_then_use",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.programs",
        "string_builder_push_str",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.raw_ptr_and_method_name",
        "string_push_str_on_mut_ok",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.reserved_names",
        "builtin_method_name_allowed",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_empty_array_len_zero",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    Entry::new(
        "cli.std_core",
        "std_core_m1_smoke",
        // `std.mem.swap` now performs a bytewise exchange through `@raw_mut`
        // (RUE-943), so the oracle model's first unsupported intrinsic for this
        // case is the address-of rather than the later `@int_to_ptr`.
        intrinsic(UnsupportedIntrinsicKind::ByteRead),
        &[],
    ),
    // std.env (RUE-935): argv/envp are captured process state, so the oracle
    // treats the `@arg_*`/`@env_*` reads as external dependencies (like
    // `@random_*`). Each case's first such read decides its registered kind.
    Entry::new(
        "cli.std_env",
        "arg_count_includes_argv0_and_passed_args",
        external(ExternalDependencyKind::ArgCount),
        &[],
    ),
    Entry::new(
        "cli.std_env",
        "arg_out_of_range_is_none",
        external(ExternalDependencyKind::ArgCount),
        &[],
    ),
    Entry::new(
        "cli.std_env",
        "args_are_echoed_in_order",
        external(ExternalDependencyKind::ArgCount),
        &[],
    ),
    Entry::new(
        "cli.std_env",
        "argv0_is_present",
        external(ExternalDependencyKind::ArgCount),
        &[],
    ),
    Entry::new(
        "cli.std_env",
        "var_absent_returns_none",
        external(ExternalDependencyKind::EnvCount),
        &[],
    ),
    Entry::new(
        "cli.std_env",
        "var_present_returns_value",
        external(ExternalDependencyKind::EnvCount),
        &[],
    ),
    Entry::new(
        "cli.std_exit",
        "std_exit_terminates_with_status",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_sort_m3",
        intrinsic(UnsupportedIntrinsicKind::ByteRead),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_strbuf_chars_share_core_decoder",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_strbuf_compiler_cutover",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_strbuf_formatting_and_text_consumers",
        runtime_call(UnsupportedRuntimeCallKind::Print),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_strings_count_chars_strict_invalid_utf8_traps",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_strings_m3",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_net_tcp",
        "tcp_connection_refused",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.std_net_tcp",
        "tcp_loopback_round_trip",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.std_strbuf",
        "mem_swap_exchanges_move_only_strbufs",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_strbuf",
        "source_owned_algorithms_use_packed_bytes",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.std_strmap",
        "std_strmap_smoke",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.strbuf_library",
        "strbuf_new_push_str_len_print",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.strbuf_library",
        "strbuf_with_capacity_concat",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.string_growth",
        "interleaved_repeated_growth_stress",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.string_mutation_receivers",
        "array_element_const_index_push",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.string_mutation_receivers",
        "array_element_dynamic_index_push",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.string_mutation_receivers",
        "field_receiver_push_str",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.string_mutation_receivers",
        "inout_param_receiver_push_str",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.string_mutation_receivers",
        "self_field_receiver_via_method",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.string_mutation_value_position",
        "statement_position_mutation_still_runs",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
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
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.try_operator",
        "try_unwrap_and_short_circuit",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.unit_fields",
        "std_option_and_arraybuf_accept_unit",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.wildcard_payload_binding",
        "discarded_non_copy_payload_drops_once",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.zst_drop_glue",
        "enum_payload_fields_keep_drop_offsets",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.zst_drop_glue",
        "nested_struct_array_keeps_drop_offsets_and_stride",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
        &[],
    ),
    Entry::new(
        "cli.zst_drop_glue",
        "struct_fields_keep_drop_offsets",
        intrinsic(UnsupportedIntrinsicKind::ByteCopy),
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
