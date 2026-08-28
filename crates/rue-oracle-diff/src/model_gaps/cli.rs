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
/// Each entry records the first unsupported semantic boundary observed by the
/// current oracle. Representation-byte support has moved affected heap cases
/// past byte copying; remaining entries are genuine runtime, syscall, pointer,
/// or layout boundaries.
const ENTRIES: &[Entry] = &[
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
    // std.fs File IO v0 (RUE-712, ADR-0057): pure-Rue fs over @syscall. The
    // oracle models the StrBuf/ArrayBuf and raw-pointer representation setup;
    // these cases remain debt because the host syscall effect is external.
    // The v0 group is ungated in the case file, so its registrations are
    // unscoped to match.
    Entry::new(
        "cli.fs_file_io",
        "fs_roundtrip",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_append",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_create_truncates_longer_existing_file",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_drop_close_reopen",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_close_then_reopen_safe",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_read_full_buffer_invalid",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_read_whole_file_loop",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_reserve_then_read_fills",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_open_missing_not_found",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_write_to_readonly",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    // std.fs v1 follow-ups (ADR-0057 "Future Work"): seek/tell, fstat/newfstatat
    // metadata, rename, unlink, and directory create/remove. Same raw-pointer
    // substrate as v0; with heap allocation and inout forwarding modeled, the
    // oracle reaches the still-unmodeled syscall substrate. These cases are
    // ungated in the case file — they pin per-target syscall numbers and struct
    // layouts, so every lane must run them (RUE-1487) — and their scope is
    // unrestricted to match.
    Entry::new(
        "cli.fs_file_io",
        "fs_seek_set_read_back",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_seek_cur_end_relative",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_stat_size_and_is_file",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_metadata_by_path_size",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_rename_old_gone_new_present",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_remove_file_then_open_notfound",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_stat_is_dir_then_rmdir",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    // RUE-995: the `create_dir_all` cases reach the same syscall boundary as the
    // rest of the std.fs group — every one of them marshals a path through
    // StrBuf, which copies bytes in bulk. The directory behavior they assert is
    // covered by their exact-stdout CLI assertions, not by the oracle model.
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_then_file_roundtrip_inside",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_all_nested_levels_usable",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_double_create_flat_errs_recursive_ok",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_all_separator_shapes",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_mkdir_all_file_in_the_way_errs",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    // RUE-1711: NUL-bearing paths are rejected rather than truncated. These
    // cases use the same modeled buffer setup as the rest of std.fs and retain
    // the group's external syscall boundary. Ungated in the case file, so
    // unscoped here to match.
    Entry::new(
        "cli.fs_file_io",
        "fs_path_with_nul_does_not_hit_the_truncated_path",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_path_with_nul_rejected_at_every_entry_point",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    // RUE-1759: the seek-offset boundary cases reach the same syscall boundary
    // as the rest of the std.fs group -- both build their path through StrBuf.
    // What they assert (that i64::MIN converts instead of trapping, and that
    // ordinary negative offsets still land exactly) is pinned by their
    // exact-stdout assertions, not by the oracle model.
    Entry::new(
        "cli.fs_file_io",
        "fs_seek_i64_min_offset_errors_instead_of_trapping",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_file_io",
        "fs_seek_negative_offsets_still_land_correctly",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    // RUE-1481: directory enumeration (`read_dir`/`walk`) reaches the same
    // syscall boundary as the rest of the std.fs group — every entry's path is
    // pooled through StrBuf, which copies bytes in bulk. Ungated in the case
    // file like the groups above, so their scope is unrestricted to match.
    //
    // The two symlink cases are absent on purpose, not overlooked: they stage
    // their directory trees as extra `files` entries, so the harness classifies
    // them ineligible ("multiple source files") and never reaches a model gap to
    // register. Adding them would fail this registry, which rejects an entry
    // that does not correspond to an observed gap.
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_empty_then_dot_entries_skipped",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_entry_path_is_openable",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_missing_and_not_a_directory_err",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_mixed_files_and_dirs_sorted",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_order_is_creation_order_independent",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_refill_loop_600_entries",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "read_dir_trailing_separator_joins_once",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_read_dir",
        "walk_nested_tree_is_depth_first_preorder",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    // RUE-1758: the inlined-continuation lowering-order cases assert on stdout
    // rather than an exit code, because lowering a use before its definition
    // produced a WRONG VALUE — a `None` read as `Some(<uninitialized>)` — not a
    // trap. `println` is therefore the first thing the oracle model cannot
    // follow in each, exactly as for the IntMap key-extreme cases below.
    Entry::new(
        "cli.inlined_continuation_lowering_order",
        "hand_written_option_none_survives_an_inlined_call",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.inlined_continuation_lowering_order",
        "multi_slot_payload_option_keeps_its_none",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.inlined_continuation_lowering_order",
        "one_byte_payload_option_keeps_its_none",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.inlined_continuation_lowering_order",
        "present_and_absent_keys_both_answer_correctly",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.inlined_continuation_lowering_order",
        "std_intmap_missing_key_stays_none_after_an_inlined_call",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    // The RUE-1636 cross-mechanism case builds a `StrBuf` and scans it byte by
    // byte, so the oracle model stops at the runtime/output boundary like every
    // other StrBuf-backed case here. The rest of the `cli.integer_class_width`
    // section is plain integer arithmetic the oracle models directly, which is
    // why this is the section's only entry.

    // RUE-954: the literal-threading regression cases invoke a real dup(2)
    // syscall, which the oracle does not model.
    Entry::new(
        "cli.intrinsic_args",
        "syscall_integer_literal_args_typed_u64",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    Entry::new(
        "cli.intrinsic_args",
        "syscall_mixed_literal_and_var_args",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    // RUE-682: only the std.hash cases that route bytes through StrBuf or
    // ArrayBuf(u8) hit the runtime/output boundary. The three that hash `str` views
    // directly — including `hash_known_answer_vectors`, which carries the
    // published FNV-1a/64 vectors — ARE modeled, so the oracle differentially
    // checks the hash arithmetic itself. That is the coverage worth having here;
    // the container spellings are asserted to agree with `str` inside the cases.

    // ADR-0052 phase 3 (RUE-974): these cases remain registered only for the
    // unsupported target-layout/unaligned outcome they exercise; field
    // projection and allocation are modeled by the oracle.
    // ADR-0052 phase 5.5 (RUE-989): the narrow-access cases retain their
    // genuine target-layout gap, rather than treating field_ptr or alloc as
    // unmodeled operations.
    // RUE-1786: the two over-rejection controls in this section are the only
    // ones that RUN -- the rest assert a compile failure and are ineligible.
    // Both build a real StrBuf, so they reach the same remaining unsupported
    // boundary as every other StrBuf-backed case here.

    // RUE-978: these byte-surface cases depend on the target-specific mapping
    // behavior noted by their registrations; @alloc itself is modeled.
    Entry::new(
        "cli.pointers",
        "ptr_offset_forward_on_mmap_pointer",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
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
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.slices",
        "slice_param_empty_array_len_zero",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    // Same empty-view debt as the spec-corpus entries (model_gaps/spec.rs):
    // an empty `[T; 0]` view has no backing place to represent, even though its
    // null pointer representation and guarded zero-length reads are modeled
    // (RUE-1610 coverage).
    Entry::new(
        "cli.slices",
        "struct_element_slice_empty_view",
        intrinsic(UnsupportedIntrinsicKind::EmptySlicePointer),
        &[],
    ),
    // std.c C-string export contract (RUE-1710): the buffer and raw-pointer
    // representation are modeled; these cases remain at the runtime/output
    // boundary. The FFI-free round-trip case reaches `println` first instead,
    // which is the runtime-call gap.
    Entry::new(
        "cli.std_c_strings",
        "c_free_c_string_returns_block_to_its_own_size_class",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_c_strings",
        "c_has_interior_nul_positions",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_c_strings",
        "c_owned_c_string_roundtrip_without_ffi",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    // IntMap key extremes (RUE-1709). These cases assert on stdout rather than
    // an exit code, because a hash that mishandles `i64::MIN` or that diverges
    // between `_slot` and `_grow_to` produces a WRONG VALUE, not a trap — so
    // `println` is the first thing the oracle model cannot follow in each.
    Entry::new(
        "cli.std_collections",
        "intmap_extreme_keys_survive_growth",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_collections",
        "intmap_key_i64_min_full_lifecycle",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_collections",
        "intmap_negative_zero_positive_and_extreme_keys",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    // std.math at the type extremes (RUE-1708). Each case reports per-width
    // answers on stdout, since the bugs produced traps and wrong booleans
    // rather than distinguishable exit codes. `math_is_prime_small_and_negative`
    // accumulates its answer into a `StrBuf` before printing, so the runtime
    // output boundary is the first unsupported effect.
    Entry::new(
        "cli.std_core",
        "math_gcd_at_type_min",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_core",
        "math_gcd_signs_zero_and_unsigned",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_core",
        "math_gcd_unrepresentable_result_panics",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_core",
        "math_is_prime_at_type_maxima",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_core",
        "math_is_prime_small_and_negative",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_core",
        "math_lcm_zero_and_type_min",
        runtime_call(UnsupportedRuntimeCallKind::Println),
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
    // std.fmt.to_radix's base contract (RUE-1707). The three in-contract cases
    // render digits to stdout, which the oracle model cannot follow past the
    // `StrBuf` copy or the `println`. `to_radix_base_1000_panics` joins them
    // because it deliberately prints a line BEFORE the guard fires; the other
    // out-of-contract cases (base 0/1/17/u64::MAX) produce no stdout at all and
    // assert only the `@panic` trap, which is a harness observation rather than
    // oracle debt, so they are absent here by design.
    Entry::new(
        "cli.std_fmt_radix",
        "to_radix_all_bases_2_through_16",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_fmt_radix",
        "to_radix_base_1000_panics",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_fmt_radix",
        "to_radix_u64_max_at_contract_edges",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_fmt_radix",
        "to_radix_zero_at_every_legal_base",
        runtime_call(UnsupportedRuntimeCallKind::Println),
        &[],
    ),
    Entry::new(
        "cli.std_m3",
        "std_strbuf_formatting_and_text_consumers",
        runtime_call(UnsupportedRuntimeCallKind::Print),
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
        "cli.std_net_tcp",
        "tcp_write_after_peer_close_is_connection_reset",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.std_strbuf",
        "mem_swap_exchanges_move_only_strbufs",
        intrinsic(UnsupportedIntrinsicKind::PointerRead),
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
        runtime_call(UnsupportedRuntimeCallKind::Println),
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
        "cli.string_alloc_failure",
        "arraybuf_growth_overflow_traps",
        intrinsic(UnsupportedIntrinsicKind::Reallocate),
        &[],
    ),
    Entry::new(
        "cli.zero_sized_place_address",
        "all_zst_root_address_is_non_null",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.zero_sized_place_address",
        "all_zst_root_projected_address",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.zero_sized_place_address",
        "indexed_zero_sized_field_address_in_bounds",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.zero_sized_place_address",
        "trailing_zero_sized_field_address_is_non_null_and_distinct",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.zero_sized_place_address",
        "trailing_zero_sized_field_address_keeps_neighbour_intact",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
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
