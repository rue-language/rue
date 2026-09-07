//! Classification and decoding of the finite intrinsic families.
//!
//! Spellings are never compared here. Each family classifies a row of the one
//! intrinsic table in `rue-builtins`, so a renamed or added intrinsic reaches
//! declaration-time comptime evaluation through the same table every other
//! phase reads.

use super::*;

use rue_builtins::IntrinsicName;

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
    /// The intrinsic row this bound is written with.
    pub const fn intrinsic_name(self) -> IntrinsicName {
        match self {
            Self::Min => IntrinsicName::IntMin,
            Self::Max => IntrinsicName::IntMax,
        }
    }

    pub fn as_str(self) -> &'static str {
        self.intrinsic_name().spelling()
    }
}

impl ComptimeTypeIntrinsic {
    /// Classify a spelling through the one intrinsic table; only the rows this
    /// family owns select a comptime type intrinsic.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::from_intrinsic(IntrinsicName::from_spelling(name)?)
    }

    /// The comptime type intrinsic a table row selects, if it is one.
    pub const fn from_intrinsic(name: IntrinsicName) -> Option<Self> {
        Some(match name {
            IntrinsicName::RequireDroppable => Self::RequireDroppable,
            IntrinsicName::RequireTriviallyDroppable => Self::RequireTriviallyDroppable,
            IntrinsicName::IntMin => Self::IntegerBound(ComptimeIntegerBound::Min),
            IntrinsicName::IntMax => Self::IntegerBound(ComptimeIntegerBound::Max),
            _ => return None,
        })
    }
}

/// The finite set of expression intrinsics whose semantic identity is known
/// to AIR.  Keeping this classification here means compiler hosts receive a
/// typed operation and do not need to rediscover the intrinsic from a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeTargetIntrinsic {
    Arch,
    Os,
    DataModel,
}

impl ComptimeTargetIntrinsic {
    /// The intrinsic row this target query is written with.
    pub const fn intrinsic_name(self) -> IntrinsicName {
        match self {
            Self::Arch => IntrinsicName::TargetArch,
            Self::Os => IntrinsicName::TargetOs,
            Self::DataModel => IntrinsicName::TargetDataModel,
        }
    }

    pub fn as_str(self) -> &'static str {
        self.intrinsic_name().spelling()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeExpressionIntrinsic {
    Import,
    Target(ComptimeTargetIntrinsic),
}

impl ComptimeExpressionIntrinsic {
    /// Classify a spelling through the one intrinsic table; only the rows this
    /// family owns select a comptime expression intrinsic.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::from_intrinsic(IntrinsicName::from_spelling(name)?)
    }

    /// The comptime expression intrinsic a table row selects, if it is one.
    pub const fn from_intrinsic(name: IntrinsicName) -> Option<Self> {
        Some(match name {
            IntrinsicName::Import => Self::Import,
            IntrinsicName::TargetArch => Self::Target(ComptimeTargetIntrinsic::Arch),
            IntrinsicName::TargetOs => Self::Target(ComptimeTargetIntrinsic::Os),
            IntrinsicName::TargetDataModel => Self::Target(ComptimeTargetIntrinsic::DataModel),
            _ => return None,
        })
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
