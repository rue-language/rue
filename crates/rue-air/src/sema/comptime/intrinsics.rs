//! Classification and decoding of the finite intrinsic families.

use super::*;

/// The finite set of type intrinsics which can participate in declaration-time
/// comptime evaluation. Classification is owned by AIR so compiler hosts do
/// not maintain a second spelling table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeTypeIntrinsic {
    RequireDroppable,
    RequireTriviallyDroppable,
    IntegerBound(ComptimeIntegerBound),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeIntegerBound {
    Min,
    Max,
}

impl ComptimeIntegerBound {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Min => "int_min",
            Self::Max => "int_max",
        }
    }
}

impl ComptimeTypeIntrinsic {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "require_droppable" => Some(Self::RequireDroppable),
            "require_trivially_droppable" => Some(Self::RequireTriviallyDroppable),
            "int_min" => Some(Self::IntegerBound(ComptimeIntegerBound::Min)),
            "int_max" => Some(Self::IntegerBound(ComptimeIntegerBound::Max)),
            _ => None,
        }
    }
}

/// The finite set of expression intrinsics whose semantic identity is known
/// to AIR.  Keeping this spelling table here means compiler hosts receive a
/// typed operation and do not need to rediscover the intrinsic from a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeTargetIntrinsic {
    Arch,
    Os,
    DataModel,
}

impl ComptimeTargetIntrinsic {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arch => "target_arch",
            Self::Os => "target_os",
            Self::DataModel => "target_data_model",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeExpressionIntrinsic {
    Import,
    Target(ComptimeTargetIntrinsic),
}

impl ComptimeExpressionIntrinsic {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "import" => Some(Self::Import),
            "target_arch" => Some(Self::Target(ComptimeTargetIntrinsic::Arch)),
            "target_os" => Some(Self::Target(ComptimeTargetIntrinsic::Os)),
            "target_data_model" => Some(Self::Target(ComptimeTargetIntrinsic::DataModel)),
            _ => None,
        }
    }
}

/// Structural facts for an expression intrinsic.  The engine decodes this
/// request before evaluating any child, so malformed controls retain their
/// declaration-time diagnostic precedence.
#[derive(Debug, Clone)]
pub enum ComptimeExpressionIntrinsicRequest<N> {
    Import {
        argument_count: usize,
        sole_string_literal: Option<N>,
    },
    Target {
        intrinsic: ComptimeTargetIntrinsic,
        argument_count: usize,
    },
}

#[derive(Debug, Clone)]
pub(super) struct DecodedComptimeExpressionIntrinsic<N> {
    pub(super) request: ComptimeExpressionIntrinsicRequest<N>,
    pub(super) site_kind: ComptimeSiteKind,
}
