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

impl<'e, H: ComptimeHost> ComptimeEngine<'e, H> {
    /// Decode the finite expression-intrinsic family before execution touches
    /// any child. This is the sole spelling and argument-shape authority.
    pub(super) fn decode_expression_intrinsic(
        &self,
        name: H::Name,
        args: &RirIntrinsicArgsRange,
    ) -> Result<DecodedComptimeExpressionIntrinsic<H::Name>, String> {
        let program = self.program_key();
        let arguments = self.program_rir().intrinsic_args(args).to_vec();
        let display_name = self.host.display_name(&name);
        let Some(intrinsic) = ComptimeExpressionIntrinsic::from_name(&display_name) else {
            return Err(display_name);
        };
        let request = match intrinsic {
            ComptimeExpressionIntrinsic::Import => {
                let sole_string_literal = (arguments.len() == 1)
                    .then(|| match self.program_rir().get(arguments[0]).data {
                        InstData::StringConst { content, .. } => {
                            Some(self.host.name_from_symbol(&program, content.into()))
                        }
                        _ => None,
                    })
                    .flatten();
                ComptimeExpressionIntrinsicRequest::Import {
                    argument_count: arguments.len(),
                    sole_string_literal,
                }
            }
            ComptimeExpressionIntrinsic::Target(target) => {
                ComptimeExpressionIntrinsicRequest::Target {
                    intrinsic: target,
                    argument_count: arguments.len(),
                }
            }
        };
        let site_kind = match &request {
            ComptimeExpressionIntrinsicRequest::Import {
                sole_string_literal: Some(_),
                ..
            } => ComptimeSiteKind::Import,
            ComptimeExpressionIntrinsicRequest::Import { .. }
            | ComptimeExpressionIntrinsicRequest::Target { .. } => ComptimeSiteKind::Intrinsic,
        };
        Ok(DecodedComptimeExpressionIntrinsic { request, site_kind })
    }
}
