//! Shared types for CFG lowering across backends.
//!
//! This module contains types used by both x86_64 and aarch64 backends
//! when tracing through field and index chains during CFG lowering.

use rue_air::ArrayTypeId;
use rue_cfg::CfgValue;

/// Result of tracing back through FieldGet chains to find the original source.
pub enum FieldChainBase {
    /// Chain originates from a Load instruction with the given slot.
    Load { slot: u32 },
    /// Chain originates from a Param instruction with the given index.
    Param { index: u32 },
}

/// Result of tracing back through IndexGet chains to find the original source.
#[derive(Clone)]
pub enum IndexChainBase {
    /// Chain originates from a Load instruction with the given slot.
    Load { slot: u32 },
    /// Chain originates from a Param instruction with the given index.
    Param { index: u32 },
    /// Chain originates from a FieldGet (array within a struct).
    FieldGet {
        struct_base_slot: u32,
        field_slot_offset: u32,
    },
}

/// Represents an index operation in a chain: the index value and the stride (slots per element).
#[derive(Clone)]
pub struct IndexLevel {
    pub index: CfgValue,
    pub elem_slot_count: u32,
    pub array_type_id: ArrayTypeId,
}
