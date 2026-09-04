//! Exact model-gap inventory for the `rue-cli-tests` corpus.

use super::{InventoryScope, ModelGapAudit, ModelGapRegistration};
use rue_oracle::{ExternalDependencyKind, ModelGapKind, SemanticGapKind, UnsupportedIntrinsicKind};
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
/// entries record the first unsupported semantic boundary observed by the
/// current oracle. Representation-byte support has moved affected heap cases
/// past byte copying; remaining entries are genuine syscall, pointer, or
/// layout boundaries.
const ENTRIES: &[Entry] = &[
    Entry::new(
        "cli.arraybuf_library",
        "arraybuf_zero_sized_element",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
        &[],
    ),
    Entry::new(
        "cli.float_codegen",
        "native_float_pointer_widths_preserve_neighbor_bytes",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
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
    // RUE-1334: recursive removal is a source-defined consumer of the same
    // directory-enumeration and filesystem-syscall boundary. The symlink case
    // stages multiple files and is therefore oracle-ineligible, like the two
    // directory-enumeration symlink cases above.
    Entry::new(
        "cli.fs_remove_dir_all",
        "remove_dir_all_empty_nested_deep_and_wide",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_remove_dir_all",
        "remove_dir_all_missing_file_and_nul_errors",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
    Entry::new(
        "cli.fs_remove_dir_all",
        "remove_dir_all_permission_denied_is_not_partial_success",
        external(ExternalDependencyKind::SystemCall),
        &[],
    ),
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
    // ADR-0052 phase 3 (RUE-974): these cases remain registered only for the
    // unsupported target-layout/unaligned outcome they exercise; field
    // projection and allocation are modeled by the oracle.
    // ADR-0052 phase 5.5 (RUE-989): the narrow-access cases retain their
    // genuine target-layout gap, rather than treating field_ptr or alloc as
    // unmodeled operations.
    // RUE-978: these byte-surface cases depend on the target-specific mapping
    // behavior noted by their registrations; @alloc itself is modeled.
    Entry::new(
        "cli.pointers",
        "ptr_offset_forward_on_mmap_pointer",
        external(ExternalDependencyKind::SystemCall),
        &["x86-64-linux"],
    ),
    // A failing `@assert_eq` renders both operands first (ADR-0083 Phase 2.5),
    // and the compiler-synthesized structural printer opens by taking a bounded
    // buffer from the allocation helper — reached as a runtime call, since the
    // printer is written directly in semantic-body form rather than in source
    // that could spell `@alloc`. The interpreter stops there, before the
    // failure-channel calls the rendering precedes, so `@alloc` is the whole
    // gap. The passing comparisons in the same section never reach it.
    Entry::new(
        "cli.rue_test_assert",
        "an_ordinary_build_names_the_inequality_it_checked",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
        &[],
    ),
    Entry::new(
        "cli.rue_test_assert",
        "an_ordinary_build_traps_with_the_pinned_message",
        intrinsic(UnsupportedIntrinsicKind::Allocate),
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
        "cli.std_net_tcp",
        "tcp_connection_refused",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux", "x86-64-linux"],
    ),
    Entry::new(
        "cli.std_net_tcp",
        "tcp_ipv6_loopback_round_trip",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux", "aarch64-macos", "x86-64-linux"],
    ),
    Entry::new(
        "cli.std_net_tcp",
        "tcp_loopback_round_trip",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-linux", "aarch64-macos", "x86-64-linux"],
    ),
    Entry::new(
        "cli.std_net_tcp",
        "tcp_macos_address_in_use_mapping",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-macos"],
    ),
    Entry::new(
        "cli.std_net_tcp",
        "tcp_macos_write_after_peer_read_shutdown",
        external(ExternalDependencyKind::SystemCall),
        &["aarch64-macos"],
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
    // Output observation is modeled now; this case still reaches its genuine
    // text parsing boundary while formatting the StrBuf consumers.
    Entry::new(
        "cli.std_m3",
        "std_strbuf_formatting_and_text_consumers",
        intrinsic(UnsupportedIntrinsicKind::ParseI32),
        &[],
    ),
    Entry::new(
        "cli.unit_fields",
        "std_option_and_arraybuf_accept_unit",
        intrinsic(UnsupportedIntrinsicKind::PointerWrite),
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
        ModelGapKind::Semantic(SemanticGapKind::FloatArithmetic) => {
            "semantic(SemanticGapKind::FloatArithmetic)".to_string()
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
